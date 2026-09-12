import { useEffect, useRef, useState } from "react";
import { commandError, type CommandError } from "./api";
import { createScenarioExportGuard, exportScenarioTimetable,
  type ScenarioExportContext, type ScenarioExportFormat, type ScenarioExportResult } from "./scenarioTimetableExportApi";

export function ScenarioTimetableExport({ context, onBusyChange }: {
  readonly context: ScenarioExportContext; readonly onBusyChange: (busy: boolean) => void;
}) {
  const guard = useRef(createScenarioExportGuard()).current;
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<ScenarioExportResult | null>(null);
  const [failure, setFailure] = useState<CommandError | null>(null);

  useEffect(() => () => { guard.invalidate(); onBusyChange(false); }, [guard, onBusyChange]);

  async function save(format: ScenarioExportFormat) {
    const token = guard.begin();
    if (token === null) return;
    setBusy(true); onBusyChange(true); setResult(null); setFailure(null);
    try {
      const response = await exportScenarioTimetable(context, format);
      if (guard.accepts(token)) setResult(response);
    } catch (error: unknown) {
      if (guard.accepts(token)) setFailure(commandError(error));
    } finally {
      if (guard.accepts(token)) {
        guard.finish(token); setBusy(false); onBusyChange(false);
      }
    }
  }

  return <section className="timetable-export" aria-label="导出当前对象的完整课表" aria-busy={busy}>
    <div className="timetable-export-actions">
      <div><strong>导出「{context.selection.label}」的完整课表</strong>
        <p>共 {context.totalRows} 课次，包含所有分页。请选择保存位置。</p></div>
      <button type="button" className="timetable-export-primary" disabled={busy} onClick={() => { void save("xlsx"); }}>导出 Excel</button>
      <button type="button" disabled={busy} onClick={() => { void save("csv"); }}>导出 CSV</button>
    </div>
    <div className="timetable-export-status" role="status" aria-live="polite">
      {busy && <p>正在准备完整课表并等待保存…</p>}
      {result?.outcome === "cancelled" && <p>已取消导出，未保存文件。</p>}
      {result?.outcome === "saved" && <p>已保存 <strong>{result.fileName}</strong> · {result.format === "xlsx" ? "Excel" : "CSV"} · 完整 {result.exportedActivityCount} 课次。
        {!result.sourceIsCurrent && <span> 此文件使用方案保存的历史来源数据。</span>}</p>}
    </div>
    {failure !== null && <div className="timetable-error" role="alert"><strong>导出结果未确认</strong>
      <p>{failure.message}</p><p>若已选择保存位置，请先检查该位置的文件。</p><code>{failure.code}</code></div>}
  </section>;
}
