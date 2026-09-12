import { useEffect, useId, useMemo, useRef, useState } from "react";
import { commandError } from "./api";
import {
  TIMETABLE_VIEWS, loadSavedTimetable, loadTimetableEntities,
  type SavedTimetablePage, type TimetableEntityPage, type TimetableRow, type TimetableView,
} from "./timetableApi";
import { loadScenarioTimetable, loadScenarioTimetableEntities, scenarioTimetableKey,
  type ScenarioTimetableEntityPage, type ScenarioTimetablePage } from "./scenarioTimetableApi";
import type { ScenarioReceipt } from "./scenarioApi";
import "./TimetableWorkspace.css";

const ENTITY_PAGE_SIZE = 20;
const ROW_PAGE_SIZE = 100;
const QUALITY_LABELS: Readonly<Record<string, string>> = {
  QUALITY_REPAIR_CHANGES: "调课变更", QUALITY_COURSE_DISTRIBUTION: "课程分布",
  QUALITY_DAILY_SUBJECT_CONCENTRATION: "同日同科集中", QUALITY_TEACHER_GAPS: "教师空堂",
  QUALITY_TEACHER_CONSECUTIVE_LOAD: "教师连续课", QUALITY_UNDESIRABLE_TIME_FAIRNESS: "不佳时段公平",
  QUALITY_STUDENT_MOVEMENT: "学生移动", QUALITY_TEACHER_MOVEMENT: "教师移动", QUALITY_LAYOUT_STABILITY: "课表稳定性",
};
const TIER_LABELS: Readonly<Record<string, string>> = { distribution: "课程分布", staff_and_movement: "教师与移动", stability: "课表稳定性", repair: "调课变更" };

type TimetableSource = { readonly kind: "run"; readonly runId: string } |
  { readonly kind: "scenario"; readonly receipt: ScenarioReceipt; readonly displayName: string; readonly sourceIsCurrent: boolean };
type EntityResult = TimetableEntityPage | ScenarioTimetableEntityPage;
type PageResult = SavedTimetablePage | ScenarioTimetablePage;

function matchesSource(source: TimetableSource, result: EntityResult | PageResult): boolean {
  return source.kind === "run" ? "runId" in result && result.runId === source.runId
    : "receipt" in result && scenarioTimetableKey(result.receipt) === scenarioTimetableKey(source.receipt);
}

export function TimetableWorkspace({ runId }: { readonly runId: string }) {
  const source = useMemo<TimetableSource>(() => ({ kind: "run", runId }), [runId]);
  return <ReadOnlyTimetableWorkspace key={`run:${runId}`} source={source} />;
}

export function ScenarioTimetableWorkspace({ receipt, displayName, sourceIsCurrent }: {
  readonly receipt: ScenarioReceipt; readonly displayName: string; readonly sourceIsCurrent: boolean;
}) {
  const source = useMemo<TimetableSource>(() => ({ kind: "scenario", receipt, displayName, sourceIsCurrent }),
    [receipt, displayName, sourceIsCurrent]);
  return <ReadOnlyTimetableWorkspace key={`scenario:${scenarioTimetableKey(receipt)}`} source={source} />;
}

function ReadOnlyTimetableWorkspace({ source }: { readonly source: TimetableSource }) {
  const labelId = useId();
  const [view, setView] = useState<TimetableView>("administrative_class");
  const [entityOffset, setEntityOffset] = useState(0);
  const [entities, setEntities] = useState<EntityResult | null>(null);
  const [entityId, setEntityId] = useState("");
  const [rowOffset, setRowOffset] = useState(0);
  const [page, setPage] = useState<{ readonly view: TimetableView; readonly data: PageResult } | null>(null);
  const [loadingEntities, setLoadingEntities] = useState(false);
  const [loadingRows, setLoadingRows] = useState(false);
  const [failure, setFailure] = useState<ReturnType<typeof commandError> | null>(null);
  const [inspectedId, setInspectedId] = useState<string | null>(null);
  const inspectedHeading = useRef<HTMLHeadingElement>(null);
  const inspectedTrigger = useRef<HTMLButtonElement | null>(null);

  useEffect(() => {
    let stale = false;
    setLoadingEntities(true); setEntities(null); setEntityId(""); setPage(null);
    setRowOffset(0); setInspectedId(null); setFailure(null);
    const query = source.kind === "run" ? loadTimetableEntities(source.runId, view, entityOffset, ENTITY_PAGE_SIZE)
      : loadScenarioTimetableEntities(source.receipt, view, entityOffset, ENTITY_PAGE_SIZE);
    void query.then((result) => {
      if (!stale) setEntities(result);
    }).catch((error: unknown) => {
      if (!stale) setFailure(commandError(error));
    }).finally(() => { if (!stale) setLoadingEntities(false); });
    return () => { stale = true; };
  }, [source, view, entityOffset]);

  useEffect(() => {
    let stale = false;
    setPage(null); setInspectedId(null); setFailure(null);
    if (!entityId) { setLoadingRows(false); return () => { stale = true; }; }
    setLoadingRows(true);
    const query = source.kind === "run" ? loadSavedTimetable(source.runId, view, entityId, rowOffset, ROW_PAGE_SIZE)
      : loadScenarioTimetable(source.receipt, view, entityId, rowOffset, ROW_PAGE_SIZE);
    void query.then((result) => {
      if (!stale) setPage({ view, data: result });
    }).catch((error: unknown) => {
      if (!stale) setFailure(commandError(error));
    }).finally(() => { if (!stale) setLoadingRows(false); });
    return () => { stale = true; };
  }, [source, view, entityId, rowOffset]);

  useEffect(() => { if (inspectedId) inspectedHeading.current?.focus(); }, [inspectedId]);

  const currentEntities = entities && matchesSource(source, entities) && entities.view === view && entities.offset === entityOffset ? entities : null;
  const currentPage = page && matchesSource(source, page.data) && page.view === view && page.data.selection.id === entityId && page.data.offset === rowOffset ? page.data : null;
  const scenarioMetadata = currentPage && "receipt" in currentPage ? currentPage
    : currentEntities && "receipt" in currentEntities ? currentEntities : null;
  const historicalSource = source.kind === "scenario" && !(scenarioMetadata?.sourceIsCurrent ?? source.sourceIsCurrent);
  const title = source.kind === "scenario" ? scenarioMetadata?.scenarioDisplayName ?? source.displayName : "课表工作台";
  const rowsById = useMemo(() => new Map(currentPage?.rows.map((row) => [row.activityId, row]) ?? []), [currentPage]);
  const calendar = currentPage?.calendar ?? [];
  const days = [...new Map(calendar.map((cell) => [cell.day, { day: cell.day, label: cell.dayLabel }])).values()];
  const periods = [...new Map(calendar.map((cell) => [cell.periodIndex, {
    index: cell.periodIndex, label: cell.periodLabel, block: cell.instructionalBlock,
  }])).values()];
  const cells = new Map(calendar.map((cell) => [`${cell.day}:${cell.periodIndex}`, cell]));
  const inspected = inspectedId ? rowsById.get(inspectedId) : undefined;
  const viewLabel = TIMETABLE_VIEWS.find((item) => item.value === view)?.label ?? "课表";

  function inspect(row: TimetableRow, trigger: HTMLButtonElement) {
    inspectedTrigger.current = trigger;
    setInspectedId(row.activityId);
  }

  function closeInspector() {
    setInspectedId(null);
    inspectedTrigger.current?.focus();
  }

  return <section className="timetable-workspace" aria-labelledby={`${labelId}-title`}>
    <header className="timetable-heading">
      <div><span className="timetable-eyebrow">{source.kind === "scenario" ? "已采用方案 · 课表" : "已保存运行"}</span><h2 id={`${labelId}-title`}>{title}</h2>
        {source.kind === "scenario" && <p className="timetable-version">方案版本 {source.receipt.scenarioRevision} · 课表版本 {source.receipt.timetableRevision}</p>}</div>
      <span className="timetable-readonly">只读查看</span>
    </header>
    <p className="timetable-intro">{source.kind === "scenario"
      ? "查看此方案保存的课表。每次查询均按方案的历史输入、成班和课表重新校验；当前为只读视图。"
      : "查看原始版本的排课结果。打开时会重新校验；原运行与采用后的独立方案分别保存。"}</p>
    {historicalSource && <p className="timetable-history-warning" role="status">源项目已有更新。这里显示此方案保存的历史数据和课表，不会自动重排或改写方案。</p>}
    <div className="timetable-controls">
      <label htmlFor={`${labelId}-view`}>查看方式
        <select id={`${labelId}-view`} value={view} onChange={(event) => {
          const selected = TIMETABLE_VIEWS.find((item) => item.value === event.target.value);
          if (selected) { setView(selected.value); setEntityOffset(0); setEntityId(""); setRowOffset(0); }
        }}>
          {TIMETABLE_VIEWS.map((item) => <option key={item.value} value={item.value}>{item.label}</option>)}
        </select>
      </label>
      <label htmlFor={`${labelId}-entity`}>选择{viewLabel}
        <select id={`${labelId}-entity`} disabled={loadingEntities || !currentEntities?.entities.length} value={entityId}
          onChange={(event) => { setEntityId(event.target.value); setRowOffset(0); }}>
          <option value="">请选择{viewLabel}</option>
          {currentEntities?.entities.map((entity) => <option key={entity.id} value={entity.id}>{entity.label} · {entity.code}</option>)}
        </select>
      </label>
      <div className="timetable-pagination" aria-label={`${viewLabel}对象分页`}>
        <button type="button" disabled={loadingEntities || entityOffset === 0} onClick={() => setEntityOffset(Math.max(0, entityOffset - ENTITY_PAGE_SIZE))}>上一页对象</button>
        <span>{currentEntities ? `${currentEntities.totalEntities} 个对象` : "读取对象…"}</span>
        <button type="button" disabled={loadingEntities || !currentEntities?.hasMore} onClick={() => {
          if (currentEntities?.nextOffset !== null && currentEntities?.nextOffset !== undefined) setEntityOffset(currentEntities.nextOffset);
        }}>下一页对象</button>
      </div>
    </div>
    <div className="timetable-status" aria-live="polite" role="status">
      {loadingEntities ? "正在读取此视图的对象…" : loadingRows ? "正在校验并读取已保存课表…" :
        currentPage ? `${currentPage.selection.label}：共 ${currentPage.totalRows} 课次，当前显示 ${currentPage.rows.length} 课次。` :
        !failure ? `选择一个${viewLabel}打开课表。` : null}
    </div>
    {failure && <div className="timetable-error" role="alert"><strong>无法读取课表</strong><p>{failure.message}</p><code>{failure.code}</code></div>}
    {currentPage && <>
      <div className="timetable-result-meta"><strong>{currentPage.selection.label}</strong><span>来源版本 {"receipt" in currentPage ? currentPage.receipt.sourceProjectRevision : currentPage.projectRevision}</span>
        <span>独立校验通过</span>{"selectedAttemptIndex" in currentPage && currentPage.selectedAttemptIndex !== null && <span>本次成班候选 {currentPage.selectedAttemptIndex + 1}</span>}</div>
      <div className="timetable-grid-scroll" tabIndex={0} role="region" aria-label={`${currentPage.selection.label}周课表，可横向滚动`}>
        <table className="timetable-grid">
          <caption>{"receipt" in currentPage ? `${currentPage.scenarioDisplayName} · ` : ""}{currentPage.selection.label} · 周课表</caption>
          <thead><tr><th scope="col">课节</th>{days.map((entry) => <th scope="col" key={entry.day}>{entry.label}</th>)}</tr></thead>
          <tbody>{periods.map((period, index) => <tr key={period.index} className={index > 0 && periods[index - 1]?.block !== period.block ? "timetable-break" : undefined}>
            <th scope="row">{period.label}</th>
            {days.map((entry) => {
              const cell = cells.get(`${entry.day}:${period.index}`);
              return <td key={entry.day}>
                {cell?.pageActivityIds.map((id) => {
                  const row = rowsById.get(id);
                  return row ? <button type="button" className="timetable-lesson" key={id}
                    aria-label={`查看${row.coursePlan.label}，${cell.dayLabel}${cell.periodLabel}，${row.audience.entity.label}`}
                    onClick={(event) => inspect(row, event.currentTarget)}>
                    <strong>{row.coursePlan.label}{cell.timeslotIndex !== row.startTimeslotIndex ? " · 续" : ""}</strong>
                    <span>{row.audience.entity.label}</span><span>{row.teacher.label} · {row.room.label}</span>
                  </button> : null;
                })}
                {cell && cell.occupiedCount > cell.pageActivityIds.length && <span className="timetable-other-page">另有 {cell.occupiedCount - cell.pageActivityIds.length} 课次在其他页</span>}
                {cell?.occupiedCount === 0 && <span className="timetable-empty">无课程</span>}
              </td>;
            })}
          </tr>)}</tbody>
        </table>
      </div>
      <p className="timetable-grid-note">周格保留全部课节。年级等综合视图可能有并行课次；“其他页”表示课次尚未加载到当前页。</p>
      <div className="timetable-pagination timetable-row-pagination" aria-label="课次分页">
        <button type="button" disabled={loadingRows || rowOffset === 0} onClick={() => setRowOffset(Math.max(0, rowOffset - ROW_PAGE_SIZE))}>上一页课次</button>
        <span>{currentPage.totalRows === 0 ? "没有匹配课次" : `${rowOffset + 1}–${rowOffset + currentPage.rows.length} / ${currentPage.totalRows} 课次`}</span>
        <button type="button" disabled={loadingRows || !currentPage.hasMore} onClick={() => { if (currentPage.nextOffset !== null) setRowOffset(currentPage.nextOffset); }}>下一页课次</button>
      </div>
      <div className="timetable-details-layout">
        <div className="timetable-list-scroll" tabIndex={0} role="region" aria-label="当前页课次列表">
          <table className="timetable-list"><caption>当前页课次</caption>
            <thead><tr><th scope="col">时间</th><th scope="col">课程 / 授课对象</th><th scope="col">教师</th><th scope="col">教室</th><th scope="col">人数</th><th scope="col">详情</th></tr></thead>
            <tbody>{currentPage.rows.map((row) => <tr key={row.activityId}>
              <td>{row.dayLabel} {row.periodLabel}<small>{row.durationPeriods} 课时</small></td>
              <td>{row.coursePlan.label}<small>{row.audience.entity.label}</small></td><td>{row.teacher.label}</td><td>{row.room.label}</td><td>{row.studentCount}</td>
              <td><button type="button" onClick={(event) => inspect(row, event.currentTarget)} aria-label={`查看${row.coursePlan.label}第${row.meetingOrdinal}次课详情`}>查看</button></td>
            </tr>)}</tbody>
          </table>
        </div>
        {inspected && <aside className="timetable-inspector" aria-labelledby={`${labelId}-detail-title`}>
          <div className="timetable-inspector-heading"><h3 id={`${labelId}-detail-title`} tabIndex={-1} ref={inspectedHeading}>{inspected.coursePlan.label}</h3>
            <button type="button" onClick={closeInspector} aria-label="关闭课次详情">关闭</button></div>
          <dl><dt>时间</dt><dd>{inspected.dayLabel} {inspected.periodLabel}起 · {inspected.durationPeriods}课时</dd>
            <dt>授课对象</dt><dd>{inspected.audience.entity.label} · {inspected.studentCount}人</dd>
            <dt>教师 / 教室</dt><dd>{inspected.teacher.label} / {inspected.room.label}</dd>
            <dt>课程计划 / 学科</dt><dd>{inspected.coursePlan.code} / {inspected.subject.label}</dd>
            <dt>年级</dt><dd>{inspected.grade.label}</dd><dt>课次</dt><dd>第{inspected.meetingOrdinal}次</dd>
            <dt>活动 ID</dt><dd><code>{inspected.activityId}</code></dd></dl>
        </aside>}
      </div>
      <details className="timetable-quality"><summary>查看整份课表的质量评分与来源</summary>
        <p>以下是 Rust 重载复核后的整份课表评分，未按当前视图重新计算。各层级按优先顺序比较，数值越低越好。</p>
        <ol>{currentPage.quality.map((tier) => <li key={tier.id}><strong>{TIER_LABELS[tier.id] ?? tier.id}：{tier.value}</strong><ul>
          {tier.metrics.map((metric) => <li key={metric.code}>{QUALITY_LABELS[metric.code] ?? metric.code}：{metric.rawValue} × {metric.weightWithinTier} = {metric.weightedValue}</li>)}
        </ul></li>)}</ol>
        {"receipt" in currentPage ? <dl>
          <dt>方案 ID / 版本</dt><dd><code>{currentPage.receipt.scenarioId}</code> / {currentPage.receipt.scenarioRevision}</dd>
          <dt>方案摘要 · BLAKE3</dt><dd><code>{currentPage.receipt.scenarioPayloadHash}</code></dd>
          <dt>课表 ID / 版本</dt><dd><code>{currentPage.receipt.timetableId}</code> / {currentPage.receipt.timetableRevision}</dd>
          <dt>课表保存内容摘要 · BLAKE3</dt><dd><code>{currentPage.receipt.timetablePayloadHash}</code></dd>
          <dt>历史导入摘要 · BLAKE3</dt><dd><code>{currentPage.receipt.sourcePayloadHash}</code></dd>
          <dt>最初采用的运行 ID</dt><dd><code>{currentPage.receipt.originRunId}</code></dd>
          <dt>最初运行产物摘要 · BLAKE3</dt><dd><code>{currentPage.receipt.originArtifactHash}</code></dd>
        </dl> : <dl><dt>运行 ID</dt><dd><code>{currentPage.runId}</code></dd><dt>原导入摘要</dt><dd><code>{currentPage.sourcePayloadHash}</code></dd>
          <dt>排课输入摘要</dt><dd><code>{currentPage.inputSnapshotHash}</code></dd><dt>输出摘要</dt><dd><code>{currentPage.outputHash}</code></dd></dl>}
      </details>
    </>}
  </section>;
}
