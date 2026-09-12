import { useEffect, useRef, useState } from "react";
import { commandError, type CommandError, type ImportedProjectReceipt } from "./api";
import type { LoadedRun } from "./solveApi";
import { ScenarioTimetableWorkspace } from "./TimetableWorkspace";
import { adoptRunAsScenario, canAdoptRun, copySavedScenario, listSavedScenarios, loadSavedScenario,
  type LoadedScenario, type ScenarioPage, type ScenarioReceipt } from "./scenarioApi";
import "./ScenarioPanel.css";

export function ScenarioPanel({ project, selectedRun, disabled }: {
  readonly project: ImportedProjectReceipt; readonly selectedRun: LoadedRun | null; readonly disabled: boolean;
}) {
  const [page, setPage] = useState<ScenarioPage | null>(null);
  const [offset, setOffset] = useState(0);
  const [refresh, setRefresh] = useState(0);
  const [listing, setListing] = useState(false);
  const [listError, setListError] = useState<CommandError | null>(null);
  const [busy, setBusy] = useState(false);
  const [loaded, setLoaded] = useState<LoadedScenario | null>(null);
  const [error, setError] = useState<CommandError | null>(null);
  const [name, setName] = useState("正式方案");
  const [copyName, setCopyName] = useState("");
  const [saved, setSaved] = useState<null | { receipt: ScenarioReceipt; name: string; action: string }>(null);
  const generation = useRef(0);
  const operation = useRef(false);
  const availableRun = selectedRun !== null && ["Feasible", "Optimal"].includes(selectedRun.run.status) && selectedRun.failureCode === null;
  const adoptable = canAdoptRun(project, selectedRun);
  const locked = disabled || busy;

  useEffect(() => () => { generation.current += 1; }, []);
  useEffect(() => {
    generation.current += 1; setLoaded(null); setOffset(0); setError(null); setRefresh((value) => value + 1);
  }, [project.revision, project.payloadHash]);
  useEffect(() => {
    let current = true;
    setListing(true); setListError(null);
    void listSavedScenarios(project.projectId, offset).then((value) => { if (current) setPage(value); })
      .catch((failure: unknown) => { if (current) setListError(commandError(failure)); })
      .finally(() => { if (current) setListing(false); });
    return () => { current = false; };
  }, [project.projectId, offset, refresh]);

  function display(value: LoadedScenario) {
    if (value.receipt.projectId !== project.projectId) throw new Error("方案不属于当前项目，请刷新列表。");
    setLoaded(value); setCopyName(`${value.displayName} · 副本`);
  }

  async function open(scenarioId: string) {
    if (operation.current || locked) return;
    operation.current = true; setBusy(true); setError(null); setSaved(null); setLoaded(null);
    const current = ++generation.current;
    try {
      const value = await loadSavedScenario(scenarioId);
      if (current === generation.current) display(value);
    } catch (failure) { if (current === generation.current) setError(commandError(failure)); }
    finally { operation.current = false; setBusy(false); }
  }

  async function create(copy: boolean) {
    if (operation.current || locked || (copy ? loaded === null || !loaded.sourceIsCurrent : !adoptable || selectedRun === null)) return;
    operation.current = true; setBusy(true); setError(null); setSaved(null);
    const current = ++generation.current;
    const displayName = copy ? copyName : name;
    try {
      const receipt = copy && loaded !== null ? await copySavedScenario(loaded, displayName)
        : selectedRun !== null ? await adoptRunAsScenario(selectedRun, displayName) : null;
      if (receipt === null || current !== generation.current) return;
      setSaved({ receipt, name: displayName, action: copy ? "已创建独立副本" : "已正式采用到" });
      setOffset(0); setRefresh((value) => value + 1); setLoaded(null);
      const value = await loadSavedScenario(receipt.scenarioId);
      if (current === generation.current) display(value);
    } catch (failure) { if (current === generation.current) setError(commandError(failure)); }
    finally { operation.current = false; setBusy(false); }
  }

  return <section className="panel scenario-panel" id="scenarios" aria-busy={busy}>
    <div className="panel-heading"><div><p className="step">明确采用 · 独立保存</p><h2>方案</h2></div>
      <button type="button" disabled={listing || busy} onClick={() => { setOffset(0); setRefresh((value) => value + 1); }}>刷新方案</button></div>
    <p className="scenario-note">采用会创建独立方案及课表版本。每次打开都会按历史输入重新核对成班、课表和品质，源项目的导入版本保持原样。</p>
    {availableRun && <div className="scenario-adopt">
      <h3>采用当前打开的运行</h3>
      <p>来源数据版本 {selectedRun.run.revision} · 运行 <code>{selectedRun.run.runId}</code></p>
      <label><span>新方案名称</span><input value={name} maxLength={200} disabled={locked} onChange={(event) => setName(event.target.value)} /></label>
      <button type="button" className="primary-button" disabled={locked || !adoptable || name.trim().length === 0} onClick={() => { void create(false); }}>采用为新方案</button>
      {!adoptable && <p className="scenario-note">该运行的来源与当前打开项目不一致，请打开当前数据版本的成功运行后采用。</p>}
    </div>}
    {!availableRun && <p className="scenario-note">从运行历史打开一份成功课表后，可在这里采用为新方案。</p>}
    {saved !== null && <div className="scenario-saved" role="status"><strong>{saved.action}「{saved.name}」</strong>
      <span>方案版本 {saved.receipt.scenarioRevision} · 课表版本 {saved.receipt.timetableRevision}</span><code>{saved.receipt.scenarioId}</code></div>}
    {busy && <p role="status">正在保存或独立复核方案…</p>}
    {error !== null && <div className="solve-error" role="alert">{saved !== null && <p>保存凭据已收到，方案已创建；以下错误来自后续打开复核。</p>}<p>{error.message}</p><code>{error.code}</code></div>}
    {listError !== null && <div className="solve-error" role="alert"><p>{listError.message}</p><code>{listError.code}</code></div>}
    {listing && <p role="status">正在读取方案列表…</p>}
    {!listing && page?.scenarios.length === 0 && <p className="scenario-note">此项目还没有已采用的方案。</p>}
    <div className="scenario-list">{page?.scenarios.map((item) => <article key={item.scenarioId} className={loaded?.receipt.scenarioId === item.scenarioId ? "selected" : ""}>
      <div><strong>{item.displayName}</strong><span>方案 {item.scenarioRevision} · 课表 {item.timetableRevision} · 来源数据 {item.sourceProjectRevision}</span>
        <time dateTime={item.createdAt}>{new Date(item.createdAt).toLocaleString("zh-CN", { hour12: false })}</time></div>
      <button type="button" disabled={locked || !item.openable} onClick={() => { void open(item.scenarioId); }}>{item.openable ? "打开并复核" : "标识不受支持"}</button>
    </article>)}</div>
    {(offset > 0 || page?.hasMore) && <div className="project-pagination"><button type="button" disabled={offset === 0 || listing || busy} onClick={() => setOffset(Math.max(0, offset - 20))}>上一页</button>
      <span>第 {Math.floor(offset / 20) + 1} 页</span><button type="button" disabled={page?.nextOffset == null || listing || busy} onClick={() => { if (page?.nextOffset != null) setOffset(page.nextOffset); }}>下一页</button></div>}
    {loaded !== null && <section className="scenario-detail" aria-label="已复核方案详情">
      <h3>{loaded.displayName}</h3><p>已独立复核 · {loaded.activityCount} 次课 · 打开时来源{loaded.sourceIsCurrent ? "为当前数据版本" : "为历史数据版本"}</p>
      <p>方案版本 {loaded.receipt.scenarioRevision} · 课表版本 {loaded.receipt.timetableRevision} · 来源数据版本 {loaded.receipt.sourceProjectRevision}</p>
      {loaded.materializedSectionCount > 0 && <p>此方案保存了采用时选定的 {loaded.materializedSectionCount} 个教学班与 {loaded.materializedEnrollmentCount} 条学生成员关系。</p>}
      <div className="scenario-quality" aria-label="重新计算的课表品质">{loaded.quality.map((tier) => <div key={tier.id}><span>品质优先级 {tier.priority}</span><strong>{tier.value}</strong><small>违规代价，越小越好</small></div>)}</div>
      <p className="scenario-note">下方课表按本方案的独立保存内容查询。原运行课表仍可在运行历史中单独查看。</p>
      {loaded.clonedFrom !== null && <p className="scenario-note">独立复制自方案 <code>{loaded.clonedFrom.scenarioId}</code> 的方案版本 {loaded.clonedFrom.scenarioRevision} / 课表版本 {loaded.clonedFrom.timetableRevision}。</p>}
      <div className="scenario-copy"><label><span>副本名称</span><input value={copyName} maxLength={200} disabled={locked} onChange={(event) => setCopyName(event.target.value)} /></label>
        <button type="button" disabled={locked || !loaded.sourceIsCurrent || copyName.trim().length === 0} onClick={() => { void create(true); }}>创建独立副本</button></div>
      {!loaded.sourceIsCurrent && <p className="scenario-note">该方案可按历史数据查看。当前复制命令要求来源仍是当前数据版本。</p>}
      <details className="run-provenance"><summary>方案、课表与来源凭据</summary><dl>
        <dt>方案 ID</dt><dd>{loaded.receipt.scenarioId}</dd><dt>方案摘要 · BLAKE3</dt><dd>{loaded.receipt.scenarioPayloadHash}</dd>
        <dt>课表 ID</dt><dd>{loaded.receipt.timetableId}</dd><dt>课表摘要 · BLAKE3</dt><dd>{loaded.receipt.timetablePayloadHash}</dd>
        <dt>原始运行 ID</dt><dd>{loaded.receipt.originRunId}</dd><dt>原始运行摘要 · BLAKE3</dt><dd>{loaded.receipt.originArtifactHash}</dd>
        <dt>来源数据摘要 · BLAKE3</dt><dd>{loaded.receipt.sourcePayloadHash}</dd></dl></details>
      <ScenarioTimetableWorkspace receipt={loaded.receipt} displayName={loaded.displayName} sourceIsCurrent={loaded.sourceIsCurrent} />
    </section>}
  </section>;
}
