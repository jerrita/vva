import { useState } from 'react'
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

function Chevron({ collapsed }: { collapsed: boolean }) {
  return (
    <svg
      className={`chevron${collapsed ? ' collapsed' : ''}`}
      viewBox="0 0 24 24"
      width="10"
      height="10"
      fill="none"
      stroke="currentColor"
      strokeWidth="3"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      <path d="M6 9l6 6 6-6" />
    </svg>
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
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set())

  const toggle = (project: string) => {
    setCollapsed(prev => {
      const next = new Set(prev)
      if (next.has(project)) {
        next.delete(project)
      } else {
        next.add(project)
      }
      return next
    })
  }

  return (
    <aside className="sidebar">
      <div className="sidebar-top">
        <span className="brand">vva</span>
        <button className="new-chat" onClick={onNew} title="新对话" aria-label="新对话">
          <svg
            viewBox="0 0 24 24"
            width="15"
            height="15"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            aria-hidden
          >
            <path d="M12 5v14M5 12h14" />
          </svg>
        </button>
      </div>
      <nav className="session-list">
        {groups.length === 0 && <div className="session-empty">暂无会话</div>}
        {groups.map(group => (
          <div key={group.project} className="session-group">
            <button
              className={`group-label${collapsed.has(group.project) ? ' closed' : ''}`}
              onClick={() => toggle(group.project)}
              title={group.project}
            >
              <Chevron collapsed={collapsed.has(group.project)} />
              <span className="group-name">{group.project}</span>
              <span className="group-count">{group.sessions.length}</span>
            </button>
            {!collapsed.has(group.project) &&
              group.sessions.map(session => (
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
