import { useEffect, useRef, useState } from "react";
import { commandError, type CommandError, type ImportedProjectReceipt } from "./api";
import { TimetableWorkspace } from "./TimetableWorkspace";
import { ScenarioPanel } from "./ScenarioPanel";
import { cancelSolveJob, listProjectRuns, loadProjectRun, querySolveJob, solveStatusLabel, startSolve,
  type LoadedRun, type RunPage, type SolveJob, type SolveSettings } from "./solveApi";
import "./SolvePanel.css";

const initialSettings: SolveSettings = { seed: "20260908", execution: "reproducible", workerCount: 1,
  timeLimitSeconds: 30, minimumSize: 10, targetSize: 40, maximumSize: 50, candidateCount: 3 };

export function SolvePanel({ project, disabled }: { readonly project: ImportedProjectReceipt; readonly disabled: boolean }) {
  const [settings, setSettings] = useState<SolveSettings>(initialSettings);
  const [starting, setStarting] = useState(false);
  const [job, setJob] = useState<SolveJob | null>(null);
  const [failure, setFailure] = useState<CommandError | null>(null);
  const [historyFailure, setHistoryFailure] = useState<CommandError | null>(null);
  const [offset, setOffset] = useState(0);
  const [refresh, setRefresh] = useState(0);
  const [page, setPage] = useState<RunPage | null>(null);
  const [historyLoading, setHistoryLoading] = useState(false);
  const [opening, setOpening] = useState<string | null>(null);
  const [selected, setSelected] = useState<LoadedRun | null>(null);
  const [pollRetry, setPollRetry] = useState(0);
  const operation = useRef(false);
  const viewGeneration = useRef(0);
  const mounted = useRef(true);
  const activeProject = useRef(project.projectId);
  const running = job?.state === "running";
  const controlsDisabled = disabled || starting || running;

  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; viewGeneration.current += 1; };
  }, []);
  useEffect(() => {
    activeProject.current = project.projectId;
    viewGeneration.current += 1;
    setOffset(0); setSelected(null); setOpening(null); setPage(null); setHistoryFailure(null);
  }, [project.projectId]);

  useEffect(() => {
    let current = true;
    setHistoryLoading(true); setHistoryFailure(null);
    void listProjectRuns(project.projectId, offset).then((value) => {
      if (current) setPage(value);
    }).catch((error: unknown) => {
      if (current) setHistoryFailure(commandError(error));
    }).finally(() => { if (current) setHistoryLoading(false); });
    return () => { current = false; };
  }, [project.projectId, offset, refresh]);

  useEffect(() => {
    if (job?.state !== "running") return;
    let current = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const id = job.jobId;
    async function poll() {
      try {
        const next = await querySolveJob(id);
        if (!current) return;
        setJob(next);
        if (next.state === "running") timer = setTimeout(() => { void poll(); }, 750);
        else { setRefresh((value) => value + 1); if (next.error !== null) setFailure(next.error); }
      } catch (error) {
        if (current) setFailure(commandError(error));
      }
    }
    void poll();
    return () => { current = false; if (timer !== undefined) clearTimeout(timer); };
  }, [job?.jobId, job?.state, pollRetry]);

  async function start() {
    if (operation.current || controlsDisabled) return;
    operation.current = true; setStarting(true); setFailure(null);
    try {
      const started = await startSolve(project, settings);
      if (mounted.current) { setJob(started); setSelected(null); }
    } catch (error) { if (mounted.current) setFailure(commandError(error)); }
    finally { operation.current = false; if (mounted.current) setStarting(false); }
  }

  async function cancel() {
    if (job?.state !== "running") return;
    try { setJob(await cancelSolveJob(job.jobId)); setFailure(null); setPollRetry((value) => value + 1); }
    catch (error) { setFailure(commandError(error)); }
  }

  async function open(runId: string) {
    const generation = ++viewGeneration.current;
    const projectId = project.projectId;
    setOpening(runId); setHistoryFailure(null); setSelected(null);
    try {
      const value = await loadProjectRun(runId);
      if (generation === viewGeneration.current && projectId === activeProject.current) setSelected(value);
    } catch (error) {
      if (generation === viewGeneration.current) setHistoryFailure(commandError(error));
    } finally { if (generation === viewGeneration.current) setOpening(null); }
  }

  function number(field: "timeLimitSeconds" | "workerCount" | "minimumSize" | "targetSize" | "maximumSize" | "candidateCount", value: string) {
    setSettings((current) => ({ ...current, [field]: Number(value) }));
  }

  return <section className="solve-workspace" id="solve">
    <div className="panel solve-panel">
      <div className="panel-heading"><div><p className="step">开始排课</p><h2>{project.displayName}</h2></div><span className="revision-chip">数据版本 {project.revision}</span></div>
      <p className="solve-note">{project.sectioningRequired ? "此项目仅保存了学生选科。请明确班额策略；系统会对候选成班逐一排课并比较。" : "使用学校导入的教学班和真实学生成员关系排课。"} 排课结果会保存为独立运行记录，打开时再次复核。</p>
      <fieldset className="solve-settings" disabled={controlsDisabled}>
        <legend>求解设置</legend>
        <label><span>每次尝试时限（秒）</span><input type="number" min="1" max="3600" value={settings.timeLimitSeconds} onChange={(event) => number("timeLimitSeconds", event.target.value)} /></label>
        <label><span>运行模式</span><select value={settings.execution} onChange={(event) => setSettings((current) => ({ ...current, execution: event.target.value as "fast" | "reproducible", workerCount: 1 }))}><option value="reproducible">可复现 · 单线程</option><option value="fast">快速 · 允许多线程</option></select></label>
        <label><span>随机种子</span><input inputMode="numeric" value={settings.seed} onChange={(event) => setSettings((current) => ({ ...current, seed: event.target.value }))} /></label>
        {settings.execution === "fast" && <label><span>线程数</span><input type="number" min="1" max="16" value={settings.workerCount} onChange={(event) => number("workerCount", event.target.value)} /></label>}
        {project.sectioningRequired && <>
          <label><span>最小班额</span><input type="number" min="1" max="65535" value={settings.minimumSize} onChange={(event) => number("minimumSize", event.target.value)} /></label>
          <label><span>目标班额</span><input type="number" min="1" max="65535" value={settings.targetSize} onChange={(event) => number("targetSize", event.target.value)} /></label>
          <label><span>最大班额</span><input type="number" min="1" max="65535" value={settings.maximumSize} onChange={(event) => number("maximumSize", event.target.value)} /></label>
          <label><span>成班候选数量</span><input type="number" min="1" max="16" value={settings.candidateCount} onChange={(event) => number("candidateCount", event.target.value)} /></label>
        </>}
      </fieldset>
      {settings.execution === "fast" && <p className="solve-note">多线程模式保留实际参数与种子，但不保证每次运行得到完全相同的课表。</p>}
      <div className="solve-actions"><button className="primary-button" type="button" disabled={controlsDisabled} onClick={() => { void start(); }}>{starting ? "正在验证项目和引擎…" : "生成课表"}</button>
        {running && <button type="button" disabled={job.cancellationRequested} onClick={() => { void cancel(); }}>{job.cancellationRequested ? "正在取消…" : "取消本次排课"}</button>}
      </div>
      {failure !== null && <div className="solve-error" role="alert"><p>{failure.message}</p><code>{failure.code}</code>{running && <button type="button" onClick={() => { setFailure(null); setPollRetry((value) => value + 1); }}>重新读取任务状态</button>}</div>}
      {job !== null && <div className={`solve-run-state ${job.state}`} role="status" aria-live="polite">
        <strong>{running ? (job.cancellationRequested ? "正在停止求解并保存终态…" : "正在排课…") : job.run !== null ? solveStatusLabel(job.run.status, job.run.terminationCode) : "任务未确认保存"}</strong>
        <span>数据版本 {job.revision} · 已用时 {(Number(job.elapsedMillis) / 1000).toFixed(1)} 秒{job.projectId !== project.projectId ? " · 属于此前打开的项目" : ""}</span>
        {job.run !== null && <span>运行已保存。{job.run.status === "Feasible" || job.run.status === "Optimal" ? "课表已通过独立必需约束校验。" : "本次未提供可用课表。"}</span>}
        {job.run !== null && job.projectId === project.projectId && <button type="button" onClick={() => { if (job.run !== null) void open(job.run.runId); }}>打开本次运行</button>}
      </div>}
    </div>

    <section className="panel run-history" aria-busy={historyLoading}>
      <div className="panel-heading"><div><p className="step">可追溯记录</p><h2>运行历史</h2></div><button type="button" disabled={historyLoading} onClick={() => { setOffset(0); setRefresh((value) => value + 1); }}>刷新</button></div>
      {historyFailure !== null && <div className="solve-error" role="alert"><p>{historyFailure.message}</p><code>{historyFailure.code}</code></div>}
      {historyLoading && <p role="status">正在读取运行记录…</p>}
      {!historyLoading && page?.runs.length === 0 && <p className="solve-note">还没有运行记录。点击“生成课表”开始第一次排课。</p>}
      {page !== null && <div className="run-list">{page.runs.map((run) => <article className={selected?.run.runId === run.runId ? "run-list-item selected" : "run-list-item"} key={run.runId}>
        <div><strong>{solveStatusLabel(run.status)}</strong><span>数据版本 {run.revision} · {new Date(run.startedAt).toLocaleString("zh-CN", { hour12: false })}</span><code>{run.runId}</code></div>
        <button type="button" disabled={opening !== null} onClick={() => { void open(run.runId); }}>{opening === run.runId ? "正在独立复核…" : "打开并复核"}</button>
      </article>)}</div>}
      {(offset > 0 || page?.hasMore) && <div className="project-pagination"><button type="button" disabled={offset === 0 || historyLoading} onClick={() => setOffset(Math.max(0, offset - 20))}>上一页</button><span>第 {Math.floor(offset / 20) + 1} 页</span><button type="button" disabled={page?.nextOffset == null || historyLoading} onClick={() => { if (page?.nextOffset != null) setOffset(page.nextOffset); }}>下一页</button></div>}
    </section>
    {selected !== null && <section id="timetable" className="saved-run-detail">
      <div className="saved-run-heading"><h2>{solveStatusLabel(selected.run.status, selected.run.terminationCode)}</h2><span>来源数据版本 {selected.run.revision} · {selected.recordedAttemptCount} 条尝试记录</span></div>
      {selected.run.revision !== project.revision && <p className="solve-note">这是历史数据版本的结果。当前项目为版本 {project.revision}。</p>}
      <p className="solve-note">这是独立保存的原始运行结果。采用操作会另外创建独立方案。</p>
      {selected.failureCode !== null && <div className="solve-error" role="alert">运行失败，未提供课表。<code>{selected.failureCode}</code></div>}
      {(selected.run.status === "Feasible" || selected.run.status === "Optimal") && <TimetableWorkspace runId={selected.run.runId} />}
      <details className="run-provenance"><summary>来源与校验记录</summary><dl><dt>运行 ID</dt><dd>{selected.run.runId}</dd><dt>来源摘要 · BLAKE3</dt><dd>{selected.run.sourcePayloadHash}</dd><dt>运行产物摘要 · BLAKE3</dt><dd>{selected.run.artifactPayloadHash}</dd><dt>终态说明代码</dt><dd>{selected.run.terminationCode}</dd></dl></details>
    </section>}
    <ScenarioPanel key={project.projectId} project={project} selectedRun={selected} disabled={disabled || starting} />
  </section>;
}
