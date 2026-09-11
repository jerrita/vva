import type { SessionSummary } from '../types'

function relativeTime(iso: string): string {
  const minutes = Math.floor((Date.now() - new Date(iso).getTime()) / 60000)
  if (minutes < 1) return '刚刚'
  if (minutes < 60) return `${minutes} 分钟前`
  const hours = Math.floor(minutes / 60)
  if (hours < 24) return `${hours} 小时前`
  const days = Math.floor(hours / 24)
  if (days < 30) return `${days} 天前`
  return new Date(iso).toLocaleDateString()
}

export function Sidebar({
  sessions,
  activeId,
  onSelect,
  onNew,
  onDelete,
}: {
  sessions: SessionSummary[]
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
        {sessions.length === 0 && <div className="session-empty">暂无会话</div>}
        {sessions.map(session => (
          <div
            key={session.id}
            className={`session-item${session.id === activeId ? ' active' : ''}`}
            onClick={() => onSelect(session.id)}
          >
            <div className="session-title">{session.title || '新对话'}</div>
            <div className="session-meta">
              <span className="session-model">{session.current_model}</span>
              <span>{relativeTime(session.created_at)}</span>
            </div>
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
      </nav>
      <div className="sidebar-foot">Vendor-Versatile Agent Harness</div>
    </aside>
  )
}
