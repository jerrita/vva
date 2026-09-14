import { useEffect, useRef, useState } from 'react'
import type { PointerEvent as ReactPointerEvent } from 'react'
import { api } from '../api'
import type { SessionSummary } from '../types'
export interface SessionGroup {
  project: string
  /** 项目 workspace 路径，用于分组内直接新建会话 */
  root: string
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
      root: list[0].workspace_root,
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

function FolderIcon({ open, color }: { open: boolean; color: string }) {
  return (
    <svg
      viewBox="0 0 24 24"
      width="14"
      height="14"
      fill="none"
      stroke={color}
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      {open ? (
        <path d="M6 14l1.5-2.9A2 2 0 0 1 9.24 10H20a2 2 0 0 1 1.94 2.5l-1.54 6a2 2 0 0 1-1.95 1.5H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3.9a2 2 0 0 1 1.69.9l.81 1.2a2 2 0 0 0 1.67.9H18a2 2 0 0 1 2 2v2" />
      ) : (
        <path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z" />
      )}
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
  const [mode, setMode] = useState<'existing' | 'new'>('existing')
  const [workspace, setWorkspace] = useState(projects[0]?.root ?? '')
  const [requestedPath, setRequestedPath] = useState('')
  const [currentPath, setCurrentPath] = useState(projects[0]?.root ?? '')
  const [parentPath, setParentPath] = useState<string | null>(null)
  const [dirs, setDirs] = useState<string[]>([])
  const [loadingDirs, setLoadingDirs] = useState(false)
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

  // 新建模式：按当前路径逐级拉取子目录
  useEffect(() => {
    if (mode !== 'new') return
    let cancelled = false
    setLoadingDirs(true)
    api
      .dirs(requestedPath)
      .then(info => {
        if (cancelled) return
        setCurrentPath(info.root)
        setParentPath(info.parent)
        setDirs(info.dirs)
      })
      .catch(() => {})
      .finally(() => {
        if (!cancelled) setLoadingDirs(false)
      })
    return () => {
      cancelled = true
    }
  }, [mode, requestedPath])

  const enterDir = (name: string) => {
    setRequestedPath(currentPath.endsWith('/') ? `${currentPath}${name}` : `${currentPath}/${name}`)
  }

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
        <div className="segmented">
          <button
            className={mode === 'existing' ? 'active' : ''}
            onClick={() => setMode('existing')}
          >
            已有项目
          </button>
          <button className={mode === 'new' ? 'active' : ''} onClick={() => setMode('new')}>
            新建项目
          </button>
        </div>
        {mode === 'existing' ? (
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
        ) : (
          <div className="new-panel-field">
            <span>项目目录（点击逐级选择）</span>
            <div className="dir-crumb">
              <button
                className="dir-up"
                disabled={!parentPath || loadingDirs}
                onClick={() => parentPath && setRequestedPath(parentPath)}
                title="上一级"
                aria-label="上一级"
              >
                <svg
                  viewBox="0 0 24 24"
                  width="13"
                  height="13"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="2"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  aria-hidden
                >
                  <path d="M12 19V5M5 12l7-7 7 7" />
                </svg>
              </button>
              <span className="dir-path" title={currentPath}>
                {currentPath}
              </span>
            </div>
            <div className="dir-list">
              {loadingDirs && <div className="dir-empty">加载中…</div>}
              {!loadingDirs && dirs.length === 0 && <div className="dir-empty">无子目录</div>}
              {dirs.map(dir => (
                <button key={dir} className="dir-item" onClick={() => enterDir(dir)}>
                  <svg
                    viewBox="0 0 24 24"
                    width="13"
                    height="13"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth="2"
                    strokeLinecap="round"
                    strokeLinejoin="round"
                    aria-hidden
                  >
                    <path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z" />
                  </svg>
                  {dir}
                </button>
              ))}
            </div>
          </div>
        )}
        <div className="new-panel-actions">
          <button className="new-cancel" onClick={onClose}>
            取消
          </button>
          <button
            className="new-create"
            disabled={mode === 'existing' ? !workspace : loadingDirs || !currentPath}
            onClick={() => onCreate(mode === 'new' ? currentPath : workspace)}
          >
            开始对话
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
  hiddenProjects,
  onHideProject,
  onResizeStart,
  onResizeMove,
  onResizeEnd,
  onSelect,
  onCreate,
  onDelete,
  onOpenSettings,
}: {
  groups: SessionGroup[]
  projects: ProjectOption[]
  activeId: string | null
  width: number
  hiddenProjects: Set<string>
  onHideProject: (project: string) => void
  onResizeStart: (event: ReactPointerEvent<HTMLDivElement>) => void
  onResizeMove: (event: ReactPointerEvent<HTMLDivElement>) => void
  onResizeEnd: (event: ReactPointerEvent<HTMLDivElement>) => void
  onSelect: (id: string) => void
  onCreate: (workspace: string) => void
  onDelete: (id: string) => void
  onOpenSettings: () => void
}) {
  // 每项目折叠状态持久化
  const [collapsed, setCollapsed] = useState<Set<string>>(
    () => new Set(JSON.parse(localStorage.getItem('vva.projects-collapsed') ?? '[]') as string[]),
  )
  const [newOpen, setNewOpen] = useState(false)
  const [pendingDelete, setPendingDelete] = useState<SessionSummary | null>(null)

  useEffect(() => {
    localStorage.setItem('vva.projects-collapsed', JSON.stringify([...collapsed]))
  }, [collapsed])

  // Esc 关闭 / Enter 确认删除
  useEffect(() => {
    if (!pendingDelete) return
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setPendingDelete(null)
      if (event.key === 'Enter') {
        onDelete(pendingDelete.id)
        setPendingDelete(null)
      }
    }
    document.addEventListener('keydown', onKeyDown)
    return () => document.removeEventListener('keydown', onKeyDown)
  }, [pendingDelete, onDelete])

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
        {groups.filter(group => !hiddenProjects.has(group.project)).length === 0 && (
          <div className="session-empty">暂无会话</div>
        )}
        {groups
          .filter(group => !hiddenProjects.has(group.project))
          .map((group, index) => (
          <div
            key={group.project}
            className="session-group"
            style={{ animationDelay: `${Math.min(index, 8) * 45}ms` }}
          >
            <div
              className={`group-label${collapsed.has(group.project) ? ' closed' : ''}`}
              onClick={() => toggle(group.project)}
              title={group.project}
            >
              <FolderIcon
                open={!collapsed.has(group.project)}
                color={projectColor(group.project)}
              />
              <span className="group-name">{group.project}</span>
              <span className="group-count">{group.sessions.length}</span>
              <button
                className="group-new"
                title={`在 ${group.project} 新建会话`}
                aria-label={`在 ${group.project} 新建会话`}
                onClick={event => {
                  event.stopPropagation()
                  onCreate(group.root)
                }}
              >
                <svg
                  viewBox="0 0 24 24"
                  width="12"
                  height="12"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="2"
                  strokeLinecap="round"
                  aria-hidden
                >
                  <path d="M12 5v14M5 12h14" />
                </svg>
              </button>
              <button
                className="group-hide"
                title="隐藏项目"
                aria-label={`隐藏项目 ${group.project}`}
                onClick={event => {
                  event.stopPropagation()
                  onHideProject(group.project)
                }}
              >
                <svg
                  viewBox="0 0 24 24"
                  width="12"
                  height="12"
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
      <button className="sidebar-foot" onClick={onOpenSettings} title="全局设置">
        Settings
      </button>
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
