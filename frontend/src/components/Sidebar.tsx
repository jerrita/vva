import { useEffect, useRef, useState } from 'react'
import type { PointerEvent as ReactPointerEvent } from 'react'
import type { SessionSummary } from '../types'
export interface SessionGroup {
  project: string
  sessions: SessionSummary[]
}

export interface ProjectOption {
  root: string
  name: string
}

/** workspace 路径末段作为项目展示名 */
export function projectName(root: string): string {
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

/** 由项目名生成稳定的高辨识度颜色（紫罗兰色系内变化，保持整体统一） */
function projectColor(name: string): string {
  let hash = 0
  for (let i = 0; i < name.length; i++) {
    hash = (hash * 31 + name.charCodeAt(i)) | 0
  }
  const hue = 238 + (Math.abs(hash) % 64)
  return `hsl(${hue} 58% 60%)`
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

function NewChatPanel({
  projects,
  onCreate,
  onClose,
}: {
  projects: ProjectOption[]
  onCreate: (workspace: string) => void
  onClose: () => void
}) {
  const [workspace, setWorkspace] = useState(projects[0]?.root ?? '')
  const panelRef = useRef<HTMLDivElement>(null)
  const anchorRef = useRef<HTMLButtonElement>(null)

  // 弹层为瞬态表单，点击外部关闭
  useEffect(() => {
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as Node
      if (panelRef.current?.contains(target) || anchorRef.current?.contains(target)) return
      onClose()
    }
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') onClose()
    }
    document.addEventListener('pointerdown', onPointerDown)
    document.addEventListener('keydown', onKeyDown)
    return () => {
      document.removeEventListener('pointerdown', onPointerDown)
      document.removeEventListener('keydown', onKeyDown)
    }
  }, [onClose])

  return (
    <>
      <button
        ref={anchorRef}
        className="new-chat"
        onClick={onClose}
        title="新对话"
        aria-label="新对话"
      >
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
      <div className="new-panel" ref={panelRef}>
        <div className="new-panel-title">新对话</div>
        <label className="new-panel-field">
          <span>项目</span>
          <select value={workspace} onChange={event => setWorkspace(event.target.value)}>
            {projects.map(project => (
              <option key={project.root} value={project.root} title={project.root}>
                {project.name}
              </option>
            ))}
          </select>
        </label>
        <div className="new-panel-path" title={workspace}>
          {workspace}
        </div>
        <div className="new-panel-actions">
          <button className="new-cancel" onClick={onClose}>
            取消
          </button>
          <button className="new-create" disabled={!workspace} onClick={() => onCreate(workspace)}>
            创建会话
          </button>
        </div>
      </div>
    </>
  )
}

export function Sidebar({
  groups,
  projects,
  activeId,
  width,
  onResizeStart,
  onResizeMove,
  onResizeEnd,
  onSelect,
  onCreate,
  onDelete,
}: {
  groups: SessionGroup[]
  projects: ProjectOption[]
  activeId: string | null
  width: number
  onResizeStart: (event: ReactPointerEvent<HTMLDivElement>) => void
  onResizeMove: (event: ReactPointerEvent<HTMLDivElement>) => void
  onResizeEnd: (event: ReactPointerEvent<HTMLDivElement>) => void
  onSelect: (id: string) => void
  onCreate: (workspace: string) => void
  onDelete: (id: string) => void
}) {
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set())
  const [newOpen, setNewOpen] = useState(false)
  const [pendingDelete, setPendingDelete] = useState<SessionSummary | null>(null)

  // Esc 关闭删除确认弹窗
  useEffect(() => {
    if (!pendingDelete) return
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setPendingDelete(null)
    }
    document.addEventListener('keydown', onKeyDown)
    return () => document.removeEventListener('keydown', onKeyDown)
  }, [pendingDelete])

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
    <aside className="sidebar" style={{ width }}>
      <div
        className="sidebar-resize"
        onPointerDown={onResizeStart}
        onPointerMove={onResizeMove}
        onPointerUp={onResizeEnd}
        onPointerCancel={onResizeEnd}
        aria-hidden
      />
      <div className="sidebar-top">
        <span className="brand">
          vva
          <span className="brand-cursor" aria-hidden />
        </span>
        {newOpen && projects.length > 0 ? (
          <NewChatPanel
            projects={projects}
            onCreate={workspace => {
              setNewOpen(false)
              onCreate(workspace)
            }}
            onClose={() => setNewOpen(false)}
          />
        ) : (
          <button
            className="new-chat"
            onClick={() => setNewOpen(true)}
            disabled={projects.length === 0}
            title="新对话"
            aria-label="新对话"
          >
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
        )}
      </div>
      <nav className="session-list">
        {groups.length === 0 && <div className="session-empty">暂无会话</div>}
        {groups.map((group, index) => (
          <div
            key={group.project}
            className="session-group"
            style={{ animationDelay: `${Math.min(index, 8) * 45}ms` }}
          >
            <button
              className={`group-label${collapsed.has(group.project) ? ' closed' : ''}`}
              onClick={() => toggle(group.project)}
              title={group.project}
            >
              <Chevron collapsed={collapsed.has(group.project)} />
              <span
                className="group-dot"
                style={{ background: projectColor(group.project) }}
                aria-hidden
              />
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
                      setPendingDelete(session)
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
      {pendingDelete && (
        <div className="modal-overlay" onClick={() => setPendingDelete(null)}>
          <div className="confirm-modal" onClick={event => event.stopPropagation()}>
            <div className="confirm-title">删除会话</div>
            <div className="confirm-message">
              确定删除「{pendingDelete.title || '新对话'}」？删除后无法恢复。
            </div>
            <div className="confirm-actions">
              <button className="ghost-button" onClick={() => setPendingDelete(null)}>
                取消
              </button>
              <button
                className="danger-button"
                onClick={() => {
                  onDelete(pendingDelete.id)
                  setPendingDelete(null)
                }}
              >
                删除
              </button>
            </div>
          </div>
        </div>
      )}
    </aside>
  )
}
