import type { SessionSummary } from '../types'

interface SessionGroup {
  project: string
  sessions: SessionSummary[]
}

/** workspace 路径末段作为项目展示名 */
function projectName(root: string): string {
  const trimmed = root.replace(/\/+$/, '')
  const name = trimmed.slice(trimmed.lastIndexOf('/') + 1)
  return name || trimmed
}

/** 按 project 分组；组内与组间均按最新会话排序 */
export function groupByProject(sessions: SessionSummary[]): SessionGroup[] {
  const groups = new Map<string, SessionSummary[]>()
  for (const session of sessions) {
    const project = projectName(session.workspace_root)
    const list = groups.get(project)
    if (list) {
      list.push(session)
    } else {
      groups.set(project, [session])
    }
  }
  return [...groups.values()]
    .map(list => ({
      project: projectName(list[0].workspace_root),
      sessions: list.sort((a, b) => b.created_at.localeCompare(a.created_at)),
    }))
    .sort(
      (a, b) =>
        b.sessions[0].created_at.localeCompare(a.sessions[0].created_at),
    )
}

export function Sidebar({
  groups,
  activeId,
  onSelect,
  onNew,
  onDelete,
}: {
  groups: SessionGroup[]
  activeId: string | null
  onSelect: (id: string) => void
  onNew: () => void
  onDelete: (id: string) => void
}) {
  return (
    <aside className="sidebar">
      <div className="sidebar-top">
        <span className="brand">vva</span>
        <button className="new-chat" onClick={onNew}>
          <svg
            viewBox="0 0 24 24"
            width="14"
            height="14"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            aria-hidden
          >
            <path d="M12 5v14M5 12h14" />
          </svg>
          新对话
        </button>
      </div>
      <nav className="session-list">
        {groups.length === 0 && <div className="session-empty">暂无会话</div>}
        {groups.map(group => (
          <div key={group.project} className="session-group">
            <div className="group-label" title={group.project}>
              {group.project}
            </div>
            {group.sessions.map(session => (
              <div
                key={session.id}
                className={`session-item${session.id === activeId ? ' active' : ''}`}
                title={session.title || '新对话'}
                onClick={() => onSelect(session.id)}
              >
                <div className="session-title">{session.title || '新对话'}</div>
                <button
                  className="session-delete"
                  title="删除会话"
                  aria-label="删除会话"
                  onClick={event => {
                    event.stopPropagation()
                    if (window.confirm('删除该会话？')) onDelete(session.id)
                  }}
                >
                  <svg
                    viewBox="0 0 24 24"
                    width="13"
                    height="13"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth="2"
                    strokeLinecap="round"
                    aria-hidden
                  >
                    <path d="M18 6L6 18M6 6l12 12" />
                  </svg>
                </button>
              </div>
            ))}
          </div>
        ))}
      </nav>
      <div className="sidebar-foot">Vendor-Versatile Agent Harness</div>
    </aside>
  )
}
