import { useEffect, useRef, useState } from "react";
import { commandError, listLocalProjects, type CommandError, type LocalProjectPage } from "./api";

export function ProjectBrowser(props: {
  readonly disabled: boolean;
  readonly activeProjectId: string | undefined;
  readonly refreshKey: string;
  readonly onOpen: (projectId: string) => void;
}) {
  const [page, setPage] = useState<LocalProjectPage | null>(null);
  const [offset, setOffset] = useState(0);
  const [refresh, setRefresh] = useState(0);
  const [loading, setLoading] = useState(true);
  const [failure, setFailure] = useState<CommandError | null>(null);
  const previousRefreshKey = useRef(props.refreshKey);

  useEffect(() => {
    if (previousRefreshKey.current !== props.refreshKey) {
      previousRefreshKey.current = props.refreshKey;
      if (offset !== 0) { setOffset(0); return; }
    }
    let current = true;
    setLoading(true);
    setFailure(null);
    void listLocalProjects(offset).then((value) => {
      if (current) setPage(value);
    }).catch((error: unknown) => {
      if (current) setFailure(commandError(error));
    }).finally(() => {
      if (current) setLoading(false);
    });
    return () => { current = false; };
  }, [offset, refresh, props.refreshKey]);

  return <section className="panel project-browser" id="projects" aria-busy={loading}>
    <div className="panel-heading"><div><p className="step">本机资料</p><h2>我的排课项目</h2></div>
      <button type="button" disabled={loading || props.disabled} onClick={() => { setOffset(0); setRefresh((value) => value + 1); }}>刷新列表</button>
    </div>
    <p className="project-list-note">从列表重新打开已保存项目，无需记住 ID。打开时 Rust 会核对摘要、版本并重新验证数据。</p>
    {failure !== null && <p className="project-list-error" role="alert">{failure.message}<small>{failure.code}</small></p>}
    {loading && <p role="status">正在读取本机项目…</p>}
    {!loading && failure === null && page?.projects.length === 0 && <p className="project-list-note">本机尚无项目。完成下方导入审计后即可新建。</p>}
    {page !== null && <div className="project-list">
      {page.projects.map((project) => <article className={props.activeProjectId === project.projectId ? "project-list-item current" : "project-list-item"} key={project.projectId}>
        <div><strong>{project.displayName}</strong><span>Revision {project.revision} · {formatTime(project.updatedAt)}</span><code>{project.projectId}</code></div>
        <button type="button" disabled={loading || props.disabled || !project.canOpen} onClick={() => props.onOpen(project.projectId)}>{project.canOpen ? "打开并验证" : "旧格式 ID，暂不可打开"}</button>
      </article>)}
    </div>}
    {page !== null && (offset > 0 || page.hasMore) && <div className="project-pagination">
      <button type="button" disabled={offset === 0 || loading || props.disabled} onClick={() => setOffset(Math.max(0, offset - 20))}>上一页</button>
      <span>第 {Math.floor(offset / 20) + 1} 页</span>
      <button type="button" disabled={page.nextOffset === null || loading || props.disabled} onClick={() => { if (page.nextOffset !== null) setOffset(page.nextOffset); }}>下一页</button>
    </div>}
  </section>;
}

function formatTime(value: string): string {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString("zh-CN", { hour12: false });
}
