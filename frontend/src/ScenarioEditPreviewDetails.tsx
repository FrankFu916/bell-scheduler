import type { ScenarioEditPreview } from "./scenarioEditApi";

const TIER_LABELS: Readonly<Record<string, string>> = { distribution: "课程分布", staff_and_movement: "教师与移动", stability: "课表稳定性", repair: "调课变更" };

export function ScenarioEditPreviewDetails({ preview }: { readonly preview: ScenarioEditPreview }) {
  const rows = new Map(preview.context.activities.map((row) => [row.activityId, row]));
  const slots = new Map(preview.context.calendar.map((slot) => [slot.timeslotId, `${slot.dayLabel} ${slot.periodLabel}`]));
  const quality = new Map(preview.afterQuality?.map((tier) => [tier.id, tier]) ?? []);
  return <div className={`scenario-edit-preview scenario-edit-${preview.status}`}>
    <div role={preview.status === "hard_rejected" ? "alert" : "status"}>
      <strong>{preview.status === "hard_rejected" ? "存在硬性冲突，不能保存" : preview.status === "no_change" ? "安排没有变化，不会产生新修订" : "全部硬性检查通过"}</strong>
      {!preview.sourceIsCurrent && <p>源项目已有更新。本预览基于历史来源，可查看结果，不能提交到当前方案。</p>}
    </div>
    {preview.changes.length > 0 && <ul className="scenario-edit-changes">{preview.changes.map((change) => {
      const row = rows.get(change.activityId);
      return <li key={change.activityId}><strong>{row?.coursePlan.label} · {row?.audience.entity.label}</strong>
        <span>{slots.get(change.before.startTimeslotId)} → {slots.get(change.after.startTimeslotId)}</span>
        {change.wasUserLocked !== change.isUserLocked && <span>{change.isUserLocked ? "将锁定此安排" : "将解除用户锁"}</span>}</li>;
    })}</ul>}
    {preview.context.totalDiagnostics > 0 && <section className="scenario-edit-diagnostics" aria-label="冲突详情">
      <h4>检查发现 {preview.context.totalDiagnostics} 项问题</h4>
      <ul>{preview.context.diagnostics.map((problem, index) => <li key={`${problem.code}:${index}`}>
        <strong>{problem.message}</strong><code>{problem.code}</code>
        <ul>{problem.activityIds.map((id) => {
          const row = rows.get(id);
          return <li key={id}>{row?.coursePlan.label} · {row?.audience.entity.label}<span>当前保存于 {row?.dayLabel} {row?.periodLabel} · {row?.teacher.label} · {row?.room.label}</span></li>;
        })}</ul>{problem.activitiesTruncated && <p>涉及课程较多，此处仅展示部分课程。</p>}
      </li>)}</ul>
      {preview.context.diagnosticsTruncated && <p>仅展示前 {preview.context.diagnostics.length} 项。完整检查已执行，仍有未展示的问题。</p>}
    </section>}
    {preview.afterQuality !== null && <section className="scenario-edit-quality" aria-label="调整前后质量比较"><h4>整份课表品质</h4>
      <p>各优先级的代价越小越好。合法调整即使代价增加，也可由你确认保存。</p>
      <table><thead><tr><th scope="col">优先级 / 项目</th><th scope="col">当前</th><th scope="col">调整后</th></tr></thead>
        <tbody>{preview.beforeQuality.map((tier) => <tr key={tier.id}><th scope="row">{tier.priority} · {TIER_LABELS[tier.id] ?? tier.id}</th>
          <td>{tier.value}</td><td>{quality.get(tier.id)?.value ?? "未返回"}</td></tr>)}</tbody></table>
    </section>}
  </div>;
}
