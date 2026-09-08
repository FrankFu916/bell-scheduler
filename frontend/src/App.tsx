import { useEffect, useRef, useState } from "react";
import { ProjectBrowser } from "./ProjectBrowser";
import { SolvePanel } from "./SolvePanel";

import {
  commandError,
  commitCsvFiles,
  inspectCsvFiles,
  loadImportedProject,
  loadImportCatalog,
  selectImportFiles,
  isWorkbookSelection,
  type CommandError,
  type InspectCsvBundleResponse,
  type InspectionConfig,
  type InspectionInputMode,
  type ImportedProjectReceipt,
  type SelectedImportFile,
  type DatasetTemplate,
} from "./api";

const initialConfig: InspectionConfig = {
  projectStableKey: "school-project",
  inputMode: "existing_sections",
  exactSubjectChoices: 3,
  periodsPerDay: 8,
  breakAfterPeriod: 4,
  minimumSize: 10,
  targetSize: 40,
  maximumSize: 50,
  seed: 20260904,
  candidateCount: 3,
};

type RunState = "idle" | "running" | "success" | "error";
type NumericConfigField = Exclude<keyof InspectionConfig, "projectStableKey" | "inputMode">;

export function App() {
  const [files, setFiles] = useState<readonly SelectedImportFile[]>([]);
  const [selectingFiles, setSelectingFiles] = useState(false);
  const [catalog, setCatalog] = useState<readonly DatasetTemplate[]>([]);
  const [catalogFailure, setCatalogFailure] = useState<CommandError | null>(null);
  const [catalogAttempt, setCatalogAttempt] = useState(0);
  const [config, setConfig] = useState<InspectionConfig>(initialConfig);
  const [runState, setRunState] = useState<RunState>("idle");
  const [result, setResult] = useState<InspectCsvBundleResponse | null>(null);
  const [failure, setFailure] = useState<CommandError | null>(null);
  const [project, setProject] = useState<ImportedProjectReceipt | null>(null);
  const [projectId, setProjectId] = useState<string>(() => crypto.randomUUID());
  const [displayName, setDisplayName] = useState("学校排课项目");
  const [commitMode, setCommitMode] = useState<"create" | "replace">("create");
  const [projectOperation, setProjectOperation] = useState<"commit" | "load" | null>(null);
  const [projectFailure, setProjectFailure] = useState<CommandError | null>(null);
  const requestToken = useRef(0);
  const requestRunning = useRef(false);
  const busy = runState === "running" || projectOperation !== null || selectingFiles;

  useEffect(() => () => {
    requestToken.current += 1;
  }, []);

  useEffect(() => {
    let current = true;
    setCatalogFailure(null);
    void loadImportCatalog().then((loaded) => {
      if (current) setCatalog(loaded);
    }).catch((error: unknown) => {
      if (current) setCatalogFailure(commandError(error));
    });
    return () => { current = false; };
  }, [catalogAttempt]);

  const selectedDatasets = files.flatMap((file) => isWorkbookSelection(file) ? file.sheetMappings.map((sheet) => sheet.dataset) : [file.dataset]);
  const missingDatasets = catalog.filter((entry) => entry.required && !selectedDatasets.includes(entry.dataset));

  async function inspect() {
    if (requestRunning.current) {
      return;
    }
    const token = requestToken.current + 1;
    requestToken.current = token;
    requestRunning.current = true;
    setRunState("running");
    setFailure(null);
    setResult(null);
    try {
      const response = await inspectCsvFiles(files, config);
      if (token !== requestToken.current) {
        return;
      }
      setResult(response);
      setRunState("success");
    } catch (error) {
      if (token !== requestToken.current) {
        return;
      }
      setFailure(commandError(error));
      setRunState("error");
    } finally {
      if (token === requestToken.current) {
        requestRunning.current = false;
      }
    }
  }

  function markDirty(change: () => void) {
    if (requestRunning.current) {
      return;
    }
    requestToken.current += 1;
    change();
    setRunState("idle");
    setResult(null);
    setFailure(null);
    setProjectFailure(null);
  }

  function updateConfig(change: (current: InspectionConfig) => InspectionConfig) {
    markDirty(() => setConfig(change));
  }

  function updateNumber(field: NumericConfigField, raw: string) {
    const parsed = Number(raw);
    updateConfig((current) => ({
      ...current,
      [field]: Number.isFinite(parsed) ? parsed : 0,
    }));
  }

  function updateMode(inputMode: InspectionInputMode) {
    updateConfig((current) => ({ ...current, inputMode }));
  }

  async function updateFiles(nextFiles: readonly File[]) {
    if (requestRunning.current || nextFiles.length === 0) return;
    markDirty(() => {});
    const token = ++requestToken.current;
    requestRunning.current = true;
    setSelectingFiles(true);
    try {
      const selected = await selectImportFiles(nextFiles, catalog);
      if (token === requestToken.current) setFiles((current) => [...current, ...selected]);
    } catch (error) {
      if (token === requestToken.current) {
        setFailure(commandError(error));
        setRunState("error");
      }
    } finally {
      if (token === requestToken.current) {
        requestRunning.current = false;
        setSelectingFiles(false);
      }
    }
  }

  function updateDataset(index: number, dataset: string, sheetIndex?: number) {
    markDirty(() => setFiles((current) => current.map((entry, position) => {
      if (position !== index) return entry;
      return isWorkbookSelection(entry)
        ? { ...entry, sheetMappings: entry.sheetMappings.map((sheet, sheetPosition) => sheetPosition === sheetIndex ? { ...sheet, dataset } : sheet) }
        : { ...entry, dataset };
    })));
  }

  function selectCommitMode(mode: "create" | "replace") {
    markDirty(() => {
      setCommitMode(mode);
      if (mode === "replace" && project !== null) {
        setProjectId(project.projectId);
        setConfig((current) => ({ ...current, projectStableKey: project.projectStableKey }));
        setDisplayName(project.displayName);
      }
    });
  }

  async function runProjectOperation(operation: "commit" | "load", selectedProjectId = projectId) {
    if (requestRunning.current || (operation === "commit" && result === null)) {
      return;
    }
    if (operation === "commit" && commitMode === "replace" && project === null) {
      return;
    }
    const token = ++requestToken.current;
    requestRunning.current = true;
    setProjectOperation(operation);
    setProjectFailure(null);
    try {
      const response = operation === "load"
        ? await loadImportedProject(selectedProjectId)
        : await commitCsvFiles(files, config, {
            projectId: commitMode === "replace" && project !== null ? project.projectId : projectId,
            displayName,
            intent: commitMode === "replace" && project !== null
              ? { mode: "replace", expectedRevision: project.revision }
              : { mode: "create" },
          });
      if (token !== requestToken.current) {
        return;
      }
      setProject(response);
      setProjectId(response.projectId);
      if (operation === "load") {
        setResult(null);
        setFailure(null);
        setRunState("idle");
        // Reloading a revision is a query; the user still explicitly selects replace.
        setCommitMode("create");
      }
    } catch (error) {
      if (token === requestToken.current) {
        setProjectFailure(commandError(error));
      }
    } finally {
      if (token === requestToken.current) {
        requestRunning.current = false;
        setProjectOperation(null);
      }
    }
  }

  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="brand">
          <span className="brand-mark" aria-hidden="true">课</span>
          <div>
            <strong>排课助手 · Bell</strong>
            <span>高中选科走班排课</span>
          </div>
        </div>
        <nav aria-label="工作台模块">
          <a className="nav-item" href="#projects">本机项目</a>
          {project !== null && <a className="nav-item" href="#solve">排课与运行历史</a>}
          <a className="nav-item" href="#import">数据导入与检查</a>
        </nav>
        <div className="sidebar-foot">
          <span className="status-dot" />
          数据保存在这台电脑
        </div>
      </aside>

      <main className="workspace">
        <header className="topbar">
          <div>
            <p className="eyebrow">排课助手 · Bell</p>
            <h1>学校排课</h1>
          </div>
          <span className="revision-chip">本机工作区</span>
        </header>

        {catalogFailure !== null && <div className="runtime-failure">
          <FailureCard failure={catalogFailure} />
          <button type="button" onClick={() => setCatalogAttempt((attempt) => attempt + 1)}>重新连接本机导入服务</button>
        </div>}
        <section className="notice" aria-label="使用步骤">
          <strong>从学校数据开始</strong>
          <span>
            导入并检查 CSV 或 Excel 数据，保存项目后开始排课。已有项目可以直接打开，查看运行历史和课表。自动成班项目在每次排课前设置班额与候选数量。
          </span>
        </section>

        {catalog.length > 0 && <ProjectBrowser disabled={busy} activeProjectId={project?.projectId} refreshKey={`${project?.projectId ?? ""}:${project?.revision ?? ""}`} onOpen={(id) => void runProjectOperation("load", id)} />}
        {project !== null && <SolvePanel project={project} disabled={busy} />}
        <div className="content-grid" id="import">
          <section className="panel setup-panel">
            <div className="panel-heading">
              <div>
                <p className="step">01</p>
                <h2>导入设置</h2>
              </div>
              <span>{files.length} 个文件</span>
            </div>

            <label className="field">
              <span>项目稳定键</span>
              <input
                value={config.projectStableKey}
                disabled={busy || commitMode === "replace"}
                onChange={(event) => {
                  const value = event.currentTarget.value;
                  updateConfig((current) => ({
                    ...current,
                    projectStableKey: value,
                  }));
                }}
              />
            </label>

            <div className="field">
              <span>教学班输入方式</span>
              <div className="segmented" role="radiogroup" aria-label="教学班输入方式">
                <button
                  type="button"
                  role="radio"
                  aria-checked={config.inputMode === "existing_sections"}
                  className={config.inputMode === "existing_sections" ? "selected" : ""}
                  disabled={busy}
                  onClick={() => updateMode("existing_sections")}
                >
                  A · 学校已成班
                </button>
                <button
                  type="button"
                  role="radio"
                  aria-checked={config.inputMode === "auto_sectioning"}
                  className={config.inputMode === "auto_sectioning" ? "selected" : ""}
                  disabled={busy}
                  onClick={() => updateMode("auto_sectioning")}
                >
                  B · 自动成班
                </button>
              </div>
            </div>

            <div className="number-grid">
              <NumberField label="每日课时" value={config.periodsPerDay} minimum={2} maximum={65_535} disabled={busy} onChange={(value) => updateNumber("periodsPerDay", value)} />
              <NumberField label="午休前最后一节" value={config.breakAfterPeriod} minimum={1} maximum={65_535} disabled={busy} onChange={(value) => updateNumber("breakAfterPeriod", value)} />
              <NumberField label="每生选科数" value={config.exactSubjectChoices} minimum={1} maximum={65_535} disabled={busy} onChange={(value) => updateNumber("exactSubjectChoices", value)} />
            </div>

            {config.inputMode === "auto_sectioning" && (
              <fieldset className="sectioning-fields">
                <legend>自动成班策略</legend>
                <div className="number-grid">
                  <NumberField label="最小班额" value={config.minimumSize} minimum={1} maximum={65_535} disabled={busy} onChange={(value) => updateNumber("minimumSize", value)} />
                  <NumberField label="目标班额" value={config.targetSize} minimum={1} maximum={65_535} disabled={busy} onChange={(value) => updateNumber("targetSize", value)} />
                  <NumberField label="最大班额" value={config.maximumSize} minimum={1} maximum={65_535} disabled={busy} onChange={(value) => updateNumber("maximumSize", value)} />
                  <NumberField label="候选数量" value={config.candidateCount} minimum={1} maximum={16} disabled={busy} onChange={(value) => updateNumber("candidateCount", value)} />
                  <NumberField label="随机种子" value={config.seed} minimum={0} maximum={Number.MAX_SAFE_INTEGER} disabled={busy} onChange={(value) => updateNumber("seed", value)} />
                </div>
              </fieldset>
            )}

            <label
              className={busy ? "file-drop disabled" : "file-drop"}
            >
              <input
                type="file"
                accept=".csv,.xlsx,text/csv,application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
                multiple
                disabled={busy || catalog.length === 0}
                onChange={(event) => {
                  void updateFiles(Array.from(event.currentTarget.files ?? []));
                  event.currentTarget.value = "";
                }}
              />
              <span className="file-icon" aria-hidden="true">表格</span>
              <strong>{selectingFiles ? "正在读取工作簿目录…" : catalog.length === 0 ? "正在连接本机导入服务…" : "添加 CSV / Excel 工作簿"}</strong>
              <small>支持 UTF-8 CSV 和 .xlsx，可多选、分批添加或混合导入。文件和工作表名称可在下方对应。</small>
            </label>

            <div className="import-guide">
              <p>{catalog.length === 0 ? "正在读取导入模板…" : missingDatasets.length === 0 ? "核心表已对应，仍需执行完整 Rust 审计。" : `还需对应：${missingDatasets.map((entry) => entry.label).join("、")}`}</p>
              <p>{config.inputMode === "existing_sections" ? "A 模式：有选科的学生还需教学班及教学班成员关系。" : "B 模式：请提供原始选科，不要同时添加已有教学班及成员关系。"}</p>
              <p>Excel 每张工作表是一类数据，第一行使用标准列名；代码保留文本格式。暂不接受公式、合并单元格、日期单元格及单元格内换行，请先整理为纯数据表。</p>
              <details><summary>查看标准列名与可选表</summary>
                {catalog.map((entry) => <div className="template-fields" key={entry.dataset}>
                  <strong>{entry.label}{entry.required ? " · 核心表" : " · 按需"}</strong>
                  <code>{entry.dataset}.csv</code>
                  <p>必需列：<code>{entry.requiredHeaders.join(", ")}</code></p>
                  {entry.optionalHeaders.length > 0 && <p>可选列：<code>{entry.optionalHeaders.join(", ")}</code></p>}
                </div>)}
              </details>
            </div>
            {files.length > 0 && <div className="file-mappings" aria-label="文件与数据表对应关系">
              {files.map((selection, index) => <div className="file-mapping" key={`${selection.file.name}-${index}`}>
                <strong>{selection.file.name}</strong><small>{selection.file.size.toLocaleString("zh-CN")} 字节</small>
                {isWorkbookSelection(selection) ? <div className="sheet-mappings">
                  {selection.sheetMappings.map((sheet, sheetIndex) => <DatasetSelect key={sheet.sheetName} label={`工作表：${sheet.sheetName}`} value={sheet.dataset} catalog={catalog} disabled={busy} onChange={(value) => updateDataset(index, value, sheetIndex)} />)}
                </div> : <DatasetSelect label={`${selection.file.name} 对应的数据表`} value={selection.dataset} catalog={catalog} disabled={busy} onChange={(value) => updateDataset(index, value)} />}
                <button type="button" disabled={busy} aria-label={`移除 ${selection.file.name}`} onClick={() => markDirty(() => setFiles((current) => current.filter((_, position) => position !== index)))}>移除</button>
              </div>)}
            </div>}

            <button
              className="primary-action"
              type="button"
              disabled={files.length === 0 || busy || selectedDatasets.some((dataset) => dataset === "")}
              onClick={() => void inspect()}
            >
              {runState === "running" ? "正在执行 Rust 审计…" : "执行导入前审计"}
            </button>
          </section>

          <section
            className="panel result-panel"
            aria-live="polite"
            aria-busy={runState === "running"}
          >
            <div className="panel-heading">
              <div>
                <p className="step">02</p>
                <h2>结构化结果</h2>
              </div>
              <ResultBadge state={runState} />
            </div>
            {result === null && failure === null && (
              <div className="empty-state">
                <div className="empty-grid" aria-hidden="true" />
                <h3>等待真实数据</h3>
                <p>结果会显示导入计数、活动规模、学生冲突边数、可复现成班记录与必需约束静态问题。</p>
              </div>
            )}
            {failure !== null && <FailureCard failure={failure} />}
            {result !== null && <InspectionResult result={result} />}
            <section className="project-commit" aria-busy={projectOperation !== null}>
              <div className="panel-heading">
                <div><p className="step">03</p><h2>写入本机项目</h2></div>
                <span>原子提交 · revision 检查</span>
              </div>
              <label className="field">
                <span>项目 ID（UUID，可粘贴已保存 ID 重新读取）</span>
                <input value={projectId} disabled={busy || commitMode === "replace"} onChange={(event) => {
                  const value = event.currentTarget.value;
                  markDirty(() => setProjectId(value));
                }} />
              </label>
              <div className="project-actions">
                <button type="button" disabled={busy || projectId.trim() === ""}
                  onClick={() => void runProjectOperation("load")}>重新读取项目 revision</button>
                <button type="button" disabled={busy} onClick={() => markDirty(() => {
                  setProjectId(crypto.randomUUID());
                  setCommitMode("create");
                })}>生成新项目 ID</button>
              </div>
              <div className="segmented" role="radiogroup" aria-label="导入写入方式">
                <button type="button" role="radio" aria-checked={commitMode === "create"}
                  className={commitMode === "create" ? "selected" : ""} disabled={busy}
                  onClick={() => selectCommitMode("create")}>新建项目</button>
                <button type="button" role="radio" aria-checked={commitMode === "replace"}
                  className={commitMode === "replace" ? "selected" : ""}
                  disabled={busy || project === null}
                  onClick={() => selectCommitMode("replace")}>替换当前项目的全部导入数据</button>
              </div>
              <label className="field">
                <span>项目显示名称</span>
                <input value={displayName} disabled={busy} onChange={(event) => {
                  const value = event.currentTarget.value;
                  markDirty(() => setDisplayName(value));
                }} />
              </label>
              {commitMode === "replace" && project !== null && <p className="commit-note">
                将完整替换 {project.projectId} 的导入数据；expected revision 为 {project.revision}。
                项目稳定键沿用已读取值。发生冲突后请重新读取并再次审计，不会自动重试。
              </p>}
              <p className="commit-note">
                提交会重新读取全部文件并由 Rust 完整审计，成功后一次写入。提交仅支持每生 3 个选考学科；B 模式仍需后续明确成班和真实排课。
              </p>
              <button className="primary-action" type="button"
                disabled={busy || result === null || displayName.trim() === "" ||
                  config.exactSubjectChoices !== 3 ||
                  !result.candidates.some((candidate) => candidate.staticValidation.passed) ||
                  (commitMode === "create" && project?.projectId === projectId)}
                onClick={() => void runProjectOperation("commit")}>
                {projectOperation === "commit" ? "正在重新审计并提交…" :
                  commitMode === "create" ? "审计后新建本机项目" : "审计后替换当前项目"}
              </button>
              {projectOperation === "load" && <p role="status">正在从 SQLite 读取并重新验证项目…</p>}
              {projectFailure !== null && <>
                <FailureCard failure={projectFailure} />
                <p className="commit-note">界面保留上次确认的项目状态。若回执中断，请使用上方项目 ID 重新读取，确认实际 revision 后再操作。</p>
              </>}
              {project !== null && <ProjectReceipt project={project} />}
            </section>
          </section>
        </div>
      </main>
    </div>
  );
}

function DatasetSelect(props: { readonly label: string; readonly value: string; readonly catalog: readonly DatasetTemplate[]; readonly disabled: boolean; readonly onChange: (value: string) => void }) {
  return <label className="dataset-select"><span>{props.label}</span>
    <select value={props.value} disabled={props.disabled} onChange={(event) => props.onChange(event.currentTarget.value)}>
      <option value="">请选择数据表类型</option>
      {props.catalog.map((entry) => <option key={entry.dataset} value={entry.dataset}>{entry.label} · {entry.dataset}</option>)}
    </select>
  </label>;
}

function NumberField(props: {
  readonly label: string;
  readonly value: number;
  readonly minimum: number;
  readonly maximum: number;
  readonly disabled: boolean;
  readonly onChange: (value: string) => void;
}) {
  return (
    <label className="field compact">
      <span>{props.label}</span>
      <input
        type="number"
        min={props.minimum}
        max={props.maximum}
        step="1"
        value={props.value}
        disabled={props.disabled}
        onChange={(event) => props.onChange(event.currentTarget.value)}
      />
    </label>
  );
}

function ResultBadge({ state }: { readonly state: RunState }) {
  const text = {
    idle: "未运行",
    running: "校验中",
    success: "已完成",
    error: "已拒绝",
  }[state];
  return <span className={`result-badge ${state}`}>{text}</span>;
}

function FailureCard({ failure }: { readonly failure: CommandError }) {
  const problems = errorProblems(failure.details);
  const details = failure.details !== null && failure.details !== undefined &&
    !(typeof failure.details === "object" && Object.keys(failure.details).length === 0);
  return (
    <div className="failure-card" role="alert">
      <p>{failure.code}</p>
      <h3>操作未完成</h3>
      <span>{failure.message}</span>
      {problems.length > 0 && <div className="problem-table-wrap"><table className="problem-table">
        <thead><tr><th>数据表 / 位置</th><th>需要修正的问题</th></tr></thead>
        <tbody>{problems.map((problem, index) => <tr key={index}>
          <td><code>{problem.dataset}</code>{problem.row !== null && <span>第 {problem.row} 行</span>}{problem.column !== null && <code>{problem.column}</code>}</td>
          <td>{problem.message}<small>{problem.code}</small></td>
        </tr>)}</tbody>
      </table></div>}
      {details && <details>
        <summary>结构化详情</summary>
        <pre>{JSON.stringify(failure.details, null, 2)}</pre>
      </details>}
    </div>
  );
}

function errorProblems(details: unknown): Array<{ dataset: string; row: string | null; column: string | null; code: string; message: string }> {
  if (typeof details !== "object" || details === null || !("problems" in details) || !Array.isArray(details.problems)) return [];
  return details.problems.flatMap((problem: unknown) => {
    if (typeof problem !== "object" || problem === null || !("code" in problem) || typeof problem.code !== "string") return [];
    const dataset = "dataset" in problem && typeof problem.dataset === "string" ? problem.dataset :
      "sheetName" in problem && typeof problem.sheetName === "string" ? `工作表：${problem.sheetName}` : "工作簿";
    return [{ dataset, code: problem.code,
      message: "message" in problem && typeof problem.message === "string" ? problem.message : "请按错误代码检查此位置的数据。",
      row: "row" in problem && (typeof problem.row === "string" || typeof problem.row === "number") ? String(problem.row) : null,
      column: "column" in problem && typeof problem.column === "string" ? problem.column :
        "column" in problem && typeof problem.column === "number" ? `第 ${problem.column} 列` : null,
    }];
  });
}

function InspectionResult({ result }: { readonly result: InspectCsvBundleResponse }) {
  const precheckPassingCandidates = result.candidates.filter(
    (candidate) => candidate.staticValidation.passed,
  ).length;
  return (
    <div className="result-stack">
      <div className="metric-grid">
        <Metric label="学生" value={result.importCounts.students} />
        <Metric label="行政班" value={result.importCounts.administrativeClasses} />
        <Metric label="选科关系" value={result.importCounts.subjectChoices} />
        <Metric label="教师" value={result.importCounts.teachers} />
        <Metric label="教室" value={result.importCounts.rooms} />
        <Metric label="课程计划" value={result.importCounts.coursePlans} />
      </div>

      {result.sectioning !== null && (
        <div className="provenance-card">
          <div>
            <span>成班算法</span>
            <strong>{result.sectioning.algorithmVersion}</strong>
          </div>
          <div>
            <span>候选</span>
            <strong>{result.sectioning.generatedCandidates} / {result.sectioning.requestedCandidates}</strong>
          </div>
          <div>
            <span>Seed</span>
            <strong>{result.sectioning.seed}</strong>
          </div>
          <code title={result.sectioning.inputHash}>{result.sectioning.inputHash.slice(0, 18)}…</code>
        </div>
      )}

      <div className="candidate-heading">
        <h3>候选静态审计</h3>
        <span>{precheckPassingCandidates} / {result.candidates.length} 未发现明显矛盾</span>
      </div>
      <div className="candidate-list">
        {result.candidates.map((candidate) => {
          const precheckPassed = candidate.staticValidation.passed;
          return (
            <article className="candidate-card" key={candidate.candidateIndex}>
              <div className="candidate-title">
                <strong>候选 {candidate.candidateIndex}</strong>
                <span className={precheckPassed ? "valid" : "invalid"}>
                  {precheckPassed
                    ? "静态检查通过"
                    : `${candidate.staticValidation.hardProblems.length} 个明显矛盾`}
                </span>
              </div>
              <dl>
                <div><dt>活动</dt><dd>{candidate.problemCounts.activities}</dd></div>
                <div><dt>冲突边</dt><dd>{candidate.problemCounts.studentConflictEdges}</dd></div>
                <div><dt>教学班</dt><dd>{candidate.candidateHash === null ? result.importCounts.teachingSections : candidate.generatedSections}</dd></div>
                <div><dt>成员关系</dt><dd>{candidate.candidateHash === null ? result.importCounts.sectionEnrollments : candidate.generatedEnrollments}</dd></div>
              </dl>
              {candidate.candidateHash !== null && <code>{candidate.candidateHash.slice(0, 24)}…</code>}
              {candidate.staticValidation.hardProblems.length > 0 && <ul className="hard-problems">
                {candidate.staticValidation.hardProblems.map((problem, index) => <li key={index}>
                  <span>{problem.message ?? "数据存在必需约束矛盾，请按问题代码检查相关活动。"}</span>
                  <strong>{problem.code}</strong>
                  <span>活动索引：{problem.activityIndices.join(", ") || "无特定活动"}</span>
                  {Object.entries(problem.parameters).map(([name, value]) => <span key={name}>{name}: {value}</span>)}
                </li>)}
              </ul>}
            </article>
          );
        })}
      </div>
    </div>
  );
}

function ProjectReceipt({ project }: { readonly project: ImportedProjectReceipt }) {
  return <div className="project-receipt">
    <h3>上次确认的本机项目</h3>
    <dl>
      <div><dt>名称</dt><dd>{project.displayName}</dd></div>
      <div><dt>项目 ID</dt><dd><code>{project.projectId}</code></dd></div>
      <div><dt>Revision</dt><dd>{project.revision}</dd></div>
      <div><dt>Document schema</dt><dd>{project.documentSchemaVersion}</dd></div>
      <div><dt>Payload hash · BLAKE3</dt><dd><code>{project.payloadHash}</code></dd></div>
      <div><dt>成班状态</dt><dd>{project.sectioningRequired ? "仍需成班：保存的是原始选科" : "使用导入的正式教学班"}</dd></div>
      <div><dt>本机 SQLite 位置</dt><dd><code>{project.databasePath}</code></dd></div>
    </dl>
    <p className="commit-note">本地数据库可能包含学生与教师信息；当前未提供数据库静态加密。导入成功尚未生成课表。</p>
  </div>;
}

function Metric({ label, value }: { readonly label: string; readonly value: number }) {
  return (
    <div className="metric">
      <span>{label}</span>
      <strong>{value.toLocaleString("zh-CN")}</strong>
    </div>
  );
}
