import { useEffect, useId, useMemo, useRef, useState } from "react";
import { commandError, type CommandError } from "./api";
import { commitScenarioEdit, previewScenarioEdit, scenarioEditKey, type ScenarioEditCommitResult,
  type ScenarioEditOperation, type ScenarioEditPreview } from "./scenarioEditApi";
import { createScenarioEditGuard, sameEditRevision, type ScenarioEditSelection } from "./scenarioEditState";
import { ScenarioEditPreviewDetails } from "./ScenarioEditPreviewDetails";
import "./ScenarioEditPanel.css";

const OPERATIONS = [
  { value: "move", label: "移动课程" }, { value: "swap_starts", label: "交换两课起点" },
  { value: "lock_current", label: "锁定当前安排" }, { value: "unlock", label: "解除用户锁" },
] as const;
type OperationKind = typeof OPERATIONS[number]["value"];

export function ScenarioEditPanel({ selection, partner, disabled, onClear, onClearPartner, onBusyChange, onCommitted }: {
  readonly selection: ScenarioEditSelection; readonly partner: ScenarioEditSelection | null; readonly disabled: boolean;
  readonly onClear: () => void; readonly onClearPartner: () => void; readonly onBusyChange: (busy: boolean) => void;
  readonly onCommitted: (result: ScenarioEditCommitResult) => Promise<void>;
}) {
  const labelId = useId();
  const [kind, setKind] = useState<OperationKind>("move");
  const [start, setStart] = useState(() => selection.calendar.find((cell) => cell.timeslotIndex === selection.row.startTimeslotIndex)?.timeslotId ?? "");
  const [lockAfter, setLockAfter] = useState(false);
  const [busy, setBusy] = useState<"preview" | "commit" | null>(null);
  const [preview, setPreview] = useState<{ readonly key: string; readonly value: ScenarioEditPreview } | null>(null);
  const [failure, setFailure] = useState<CommandError | null>(null);
  const [committed, setCommitted] = useState<ScenarioEditCommitResult | null>(null);
  const guard = useRef(createScenarioEditGuard()).current;
  const heading = useRef<HTMLHeadingElement>(null);
  const chosenPartner = partner !== null && sameEditRevision(selection, partner) && partner.row.activityId !== selection.row.activityId ? partner : null;
  const operation = useMemo<ScenarioEditOperation | null>(() => {
    const activityId = selection.row.activityId;
    if (kind === "move") return start === "" ? null : { kind, activityId, startTimeslotId: start, lockAfter };
    if (kind === "swap_starts") return chosenPartner === null ? null : { kind, leftActivityId: activityId, rightActivityId: chosenPartner.row.activityId };
    return { kind, activityId };
  }, [kind, start, lockAfter, selection.row.activityId, chosenPartner]);
  const key = operation === null ? null : scenarioEditKey(selection.receipt, operation);
  const activePreview = preview !== null && preview.key === key ? preview.value : null;
  const days = [...new Map(selection.calendar.map((cell) => [cell.day, cell.dayLabel])).entries()];
  const locked = disabled || busy !== null || committed !== null;

  useEffect(() => () => { guard.invalidate(); onBusyChange(false); }, [guard, onBusyChange]);
  useEffect(() => { heading.current?.focus(); }, []);
  useEffect(() => { guard.invalidate(); setPreview(null); setFailure(null); }, [guard, key]);
  useEffect(() => { if (chosenPartner !== null) { setKind("swap_starts"); heading.current?.focus(); } }, [chosenPartner]);

  function discardPreview() {
    guard.invalidate(); setPreview(null); setFailure(null);
  }

  async function requestPreview() {
    if (locked || operation === null || key === null) return;
    const token = guard.begin(key);
    if (token === null) return;
    setBusy("preview"); onBusyChange(true); setPreview(null); setFailure(null);
    try {
      const value = await previewScenarioEdit(selection.receipt, operation);
      if (guard.accepts(token, key)) setPreview({ key, value });
    } catch (error: unknown) {
      if (guard.accepts(token, key)) setFailure(commandError(error));
    } finally {
      if (guard.accepts(token, key)) { guard.finish(token); setBusy(null); onBusyChange(false); }
    }
  }

  async function confirm() {
    if (locked || key === null || operation === null || !activePreview?.canCommit) return;
    const token = guard.begin(key);
    if (token === null) return;
    setBusy("commit"); onBusyChange(true); setFailure(null);
    try {
      const value = await commitScenarioEdit(activePreview, operation);
      if (!guard.accepts(token, key)) return;
      // Preserve confirmed persistence before asking the parent to reload the latest view.
      setCommitted(value); setPreview(null);
      await onCommitted(value);
    } catch (error: unknown) {
      if (guard.accepts(token, key)) { setPreview(null); setFailure(commandError(error)); }
    } finally {
      if (guard.accepts(token, key)) { guard.finish(token); setBusy(null); onBusyChange(false); }
    }
  }

  return <section className="scenario-edit-panel" aria-labelledby={`${labelId}-title`} aria-busy={busy !== null}>
    <header className="scenario-edit-heading"><div><span className="timetable-eyebrow">调整已保存方案</span>
      <h3 id={`${labelId}-title`} tabIndex={-1} ref={heading}>调课与锁定</h3></div>
      <button type="button" disabled={locked} onClick={onClear}>结束本次选择</button></header>
    <div className="scenario-edit-selection"><strong>{selection.row.coursePlan.label} · {selection.row.audience.entity.label}</strong>
      <span>{selection.row.dayLabel} {selection.row.periodLabel}起 · {selection.row.durationPeriods} 课时 · {selection.row.teacher.label} · {selection.row.room.label}</span>
      <span>{selection.lock.sourceLocked ? "来源固定安排，不能通过调课解除" : selection.lock.userLocked ? "已由用户锁定；需要先预览并提交解锁，才能移动" : "当前没有用户锁"}</span></div>
    <p className="scenario-edit-note">可切换上方视图，打开另一课次详情并选择交换对象。教师、教室与连堂长度保持原安排。</p>
    <fieldset disabled={locked} className="scenario-edit-form"><legend>选择要预览的调整</legend>
      <label htmlFor={`${labelId}-operation`}>操作<select id={`${labelId}-operation`} value={kind} onChange={(event) => {
        const next = OPERATIONS.find((item) => item.value === event.target.value);
        if (next) { discardPreview(); setKind(next.value); }
      }}>{OPERATIONS.map((item) => <option key={item.value} value={item.value}>{item.label}</option>)}</select></label>
      {kind === "move" && <><label htmlFor={`${labelId}-target`}>目标起点<select id={`${labelId}-target`} value={start} onChange={(event) => { discardPreview(); setStart(event.target.value); }}>
        <option value="">请选择课节</option>{days.map(([day, label]) => <optgroup key={day} label={label}>
          {selection.calendar.filter((cell) => cell.day === day).map((cell) => <option key={cell.timeslotId} value={cell.timeslotId}>{cell.dayLabel} {cell.periodLabel}</option>)}
        </optgroup>)}</select></label>
        <label className="scenario-edit-checkbox"><input type="checkbox" checked={lockAfter} onChange={(event) => { discardPreview(); setLockAfter(event.target.checked); }} />移动后锁定此安排</label></>}
      {kind === "swap_starts" && <div className="scenario-edit-partner">{chosenPartner === null ? <p>请在任一课表视图打开另一课次详情，选择“与待调课程交换”。</p> : <>
        <strong>与 {chosenPartner.row.coursePlan.label} · {chosenPartner.row.audience.entity.label} 交换起点</strong>
        <span>{chosenPartner.row.dayLabel} {chosenPartner.row.periodLabel}起 · {chosenPartner.row.durationPeriods} 课时</span>
        <button type="button" onClick={() => { discardPreview(); onClearPartner(); }}>清除交换对象</button></>}</div>}
      {kind === "lock_current" && <p>锁定当前起点、教师和教室。重复锁定不会产生新修订。</p>}
      {kind === "unlock" && <p>只解除此课程的用户锁，来源数据中的固定安排仍然生效。</p>}
      <button type="button" className="scenario-edit-preview-button" disabled={operation === null} onClick={() => { void requestPreview(); }}>预览调整与冲突</button>
    </fieldset>
    {busy !== null && <p role="status">{busy === "preview" ? "正在检查所有课程冲突与安排要求…" : "正在重新核对并保存调课…"}</p>}
    {failure !== null && <div className="timetable-error" role="alert"><strong>调课结果未确认</strong><p>{failure.message}</p><code>{failure.code}</code>
      <p>版本冲突或提交结果未确认时，请先重新打开方案核对，再发起新的预览。</p></div>}
    {activePreview !== null && <><ScenarioEditPreviewDetails preview={activePreview} />
      <div className="scenario-edit-confirm"><button type="button" disabled={locked || !activePreview.canCommit} onClick={() => { void confirm(); }}>确认提交 · 保存新修订</button>
        <span>确认时会再次校验版本与全部安排要求。</span></div></>}
    {committed !== null && <div role="status" className="scenario-edit-confirmed">{committed.status === "committed" ? "调课已保存" : "安排没有变化"} · 方案版本 {committed.receipt.scenarioRevision} / 课表版本 {committed.receipt.timetableRevision}。正在刷新课表，已保存的版本信息会保留。</div>}
  </section>;
}
