import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type { PointerEvent as ReactPointerEvent } from 'react'
import { api } from './api'
import { subscribeSession } from './sessionSync'
import type { ConfigureBody } from './api'
import { Chat, turnText } from './components/Chat'
import { ChipSelect } from './components/ChipSelect'
import { Composer, type ComposerHandle } from './components/Composer'
import { Sidebar, groupByProject, projectName } from './components/Sidebar'
import { SettingsModal } from './components/SettingsModal'
import {
  FileSidebar,
  FILE_SIDEBAR_MIN,
  FILE_SIDEBAR_MAX,
  clampFileSidebar,
  extractRelatedFiles,
} from './components/FileSidebar'
import type { ProjectOption } from './components/Sidebar'
import type { FileAttachment, ModelsResponse, SessionSnapshot, SessionSummary, SidebarStats } from './types'

function toError(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

const SIDEBAR_MIN = 200
const SIDEBAR_MAX = 420

const clampSidebar = (width: number) =>
  Math.min(SIDEBAR_MAX, Math.max(SIDEBAR_MIN, width))

function fmtNum(value: number): string {
  return value.toLocaleString()
}

/** 上下文窗口上限缩写，对齐 TUI format_context_limit */
function fmtLimit(value: number): string {
  if (value >= 1_000_000 && value % 1_000_000 === 0) return `${value / 1_000_000}M`
  return `${Math.floor(value / 1000)}k`
}

export default function App() {
  const [models, setModels] = useState<ModelsResponse | null>(null)
  const [sessions, setSessions] = useState<SessionSummary[]>([])
  const [activeId, setActiveId] = useState<string | null>(null)
  const [snap, setSnap] = useState<SessionSnapshot | null>(null)
  const [pending, setPending] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [sidebarOpen, setSidebarOpen] = useState(false)
  const [stats, setStats] = useState<SidebarStats | null>(null)
  const [defaultRoot, setDefaultRoot] = useState<string | null>(null)
  const [sidebarHidden, setSidebarHidden] = useState(
    () => localStorage.getItem('vva.sidebar-hidden') === '1',
  )
  const [hiddenProjects, setHiddenProjects] = useState<Set<string>>(
    () =>
      new Set(
        JSON.parse(localStorage.getItem('vva.projects-hidden') ?? '[]') as string[],
      ),
  )
  const [sidebarWidth, setSidebarWidth] = useState(() => {
    const stored = Number(localStorage.getItem('vva.sidebar-width'))
    return stored >= SIDEBAR_MIN && stored <= SIDEBAR_MAX ? stored : 268
  })
  const [statsCollapsed, setStatsCollapsed] = useState(
    () => localStorage.getItem('vva.stats-collapsed') === '1',
  )
  // 遮挡自动收起：内容滚入面板区域时隐藏，离开后恢复；顶栏开关可手动覆盖
  const [statsAutoHidden, setStatsAutoHidden] = useState(false)
  const [statsPinned, setStatsPinned] = useState(false)
  const [settingsOpen, setSettingsOpen] = useState(false)
  const statsPanelRef = useRef<HTMLDivElement>(null)
  const statsOverlapRaf = useRef(0)
  const resizing = useRef(false)
  const resizeStartX = useRef(0)
  const resizeStartW = useRef(0)
  const composerRef = useRef<ComposerHandle>(null)

  const [fileSidebarOpen, setFileSidebarOpen] = useState(
    () => localStorage.getItem('vva.file-sidebar-open') === '1',
  )
  const [fileSidebarWidth, setFileSidebarWidth] = useState(() => {
    const stored = Number(localStorage.getItem('vva.file-sidebar-width'))
    return stored >= FILE_SIDEBAR_MIN && stored <= FILE_SIDEBAR_MAX ? stored : 360
  })
  const [previewFilePath, setPreviewFilePath] = useState<string | null>(null)
  const fileResizing = useRef(false)
  const fileResizeStartX = useRef(0)
  const fileResizeStartW = useRef(0)

  useEffect(() => {
    localStorage.setItem('vva.sidebar-hidden', sidebarHidden ? '1' : '0')
  }, [sidebarHidden])

  useEffect(() => {
    localStorage.setItem('vva.projects-hidden', JSON.stringify([...hiddenProjects]))
  }, [hiddenProjects])

  useEffect(() => {
    localStorage.setItem('vva.sidebar-width', String(sidebarWidth))
  }, [sidebarWidth])

  useEffect(() => {
    localStorage.setItem('vva.stats-collapsed', statsCollapsed ? '1' : '0')
  }, [statsCollapsed])

  useEffect(() => {
    localStorage.setItem('vva.file-sidebar-open', fileSidebarOpen ? '1' : '0')
  }, [fileSidebarOpen])

  useEffect(() => {
    localStorage.setItem('vva.file-sidebar-width', String(fileSidebarWidth))
  }, [fileSidebarWidth])

  const onFileResizeStart = (event: ReactPointerEvent<HTMLDivElement>) => {
    event.preventDefault()
    fileResizing.current = true
    fileResizeStartX.current = event.clientX
    fileResizeStartW.current = fileSidebarWidth
    document.body.classList.add('resizing')
    event.currentTarget.setPointerCapture(event.pointerId)
  }
  const onFileResizeMove = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (!fileResizing.current) return
    setFileSidebarWidth(
      clampFileSidebar(fileResizeStartW.current + (fileResizeStartX.current - event.clientX)),
    )
  }
  const onFileResizeEnd = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (!fileResizing.current) return
    fileResizing.current = false
    document.body.classList.remove('resizing')
    event.currentTarget.releasePointerCapture(event.pointerId)
  }

  const onPreviewFile = useCallback((path: string) => {
    setPreviewFilePath(path)
    setFileSidebarOpen(true)
  }, [])

  // 未创建会话前的模型偏好，创建会话时生效
  const [draftModel, setDraftModel] = useState<string | null>(null)
  const [draftVariant, setDraftVariant] = useState<string | null>(null)
  const [draftSearch, setDraftSearch] = useState(true)
  const [draftProject, setDraftProject] = useState('')

  const running = snap !== null && snap.state !== ''
  const active = snap?.session ?? null
  const turnCount = active?.turns.length ?? 0
  const statsVisible =
    stats !== null && active !== null && !statsCollapsed && (!statsAutoHidden || statsPinned)

  // 打开或新建位于隐藏项目的会话时，该项目重新显示
  useEffect(() => {
    if (!active) return
    const name = projectName(active.workspace_root)
    setHiddenProjects(prev => {
      if (!prev.has(name)) return prev
      const next = new Set(prev)
      next.delete(name)
      return next
    })
  }, [active?.id, active?.workspace_root])

  const scrollRef = useRef<HTMLDivElement>(null)
  const nearBottom = useRef(true)

  useEffect(() => {
    api.models().then(setModels).catch(e => setError(toError(e)))
    api.workspace().then(workspace => setDefaultRoot(workspace.root)).catch(() => {})
    api.sessions().then(setSessions).catch(e => setError(toError(e)))
  }, [])

  // 切换会话时建立 SSE 订阅，连接建立即收到当前快照
  useEffect(() => {
    if (!activeId) {
      setSnap(null)
      setPending(null)
      setPreviewFilePath(null)
      return
    }
    setSnap(null)
    setPending(null)
    setStats(null)
    setPreviewFilePath(null)
    return subscribeSession(activeId, setSnap, e => setError(toError(e)))
  }, [activeId])

  const relatedFiles = useMemo(
    () => (active ? extractRelatedFiles(active, snap?.draft) : []),
    [active, snap?.draft],
  )

  // 会话在本地会话列表中保持同步（标题 / 模型 / 轮次变化时）
  const listKey = useRef('')
  useEffect(() => {
    if (!snap || !snap.persisted) return
    const s = snap.session
    const key = `${s.id}|${s.title ?? ''}|${s.current_model}|${s.current_variant ?? ''}|${s.turns.length}`
    if (key === listKey.current) return
    listKey.current = key
    const summary: SessionSummary = {
      id: s.id,
      title: s.title ?? null,
      workspace_root: s.workspace_root,
      created_at: s.created_at,
      turn_count: s.turns.length,
      current_model: s.current_model,
      current_variant: s.current_variant ?? null,
      web_search: s.web_search,
    }
    setSessions(prev => [summary, ...prev.filter(item => item.id !== s.id)])
  }, [snap])

  // 乐观展示的用户消息在会话中出现后移除
  useEffect(() => {
    if (!pending || !snap) return
    const exists = snap.session.turns.some(
      turn => turn.role === 'user' && turnText(turn) === pending,
    )
    if (exists) setPending(null)
  }, [snap, pending])

  // 用户位于底部时跟随滚动
  useEffect(() => {
    const el = scrollRef.current
    if (el && nearBottom.current) el.scrollTop = el.scrollHeight
  }, [snap])

  // 顶栏用量数据：切换会话与轮次变化时刷新
  useEffect(() => {
    if (!activeId) {
      setStats(null)
      return
    }
    let cancelled = false
    api
      .stats(activeId)
      .then(value => {
        if (!cancelled) setStats(value)
      })
      .catch(() => {})
    return () => {
      cancelled = true
    }
  }, [activeId, turnCount])

  // 用量面板遮挡检测：任一消息内容与面板矩形相交即视为遮挡
  const refreshStatsOverlap = useCallback(() => {
    if (statsOverlapRaf.current) return
    statsOverlapRaf.current = requestAnimationFrame(() => {
      statsOverlapRaf.current = 0
      const panel = statsPanelRef.current
      const scroller = scrollRef.current
      if (!panel || !scroller) return
      const pr = panel.getBoundingClientRect()
      let overlap = false
      for (const el of scroller.querySelectorAll('.chat > *')) {
        const r = el.getBoundingClientRect()
        if (
          Math.min(pr.right, r.right) - Math.max(pr.left, r.left) > 8 &&
          Math.min(pr.bottom, r.bottom) - Math.max(pr.top, r.top) > 8
        ) {
          overlap = true
          break
        }
      }
      setStatsAutoHidden(overlap)
      if (!overlap) setStatsPinned(false)
    })
  }, [])

  // 内容变化（流式增量、切换会话、手动开合）后重新评估遮挡
  useEffect(() => {
    refreshStatsOverlap()
  }, [snap, statsCollapsed, statsPinned, refreshStatsOverlap])

  // 视口与布局尺寸变化（窗口缩放、侧栏拖宽）后重新评估遮挡
  useEffect(() => {
    if (!activeId) return
    const scroller = scrollRef.current
    if (!scroller) return
    const observer = new ResizeObserver(refreshStatsOverlap)
    observer.observe(scroller)
    window.addEventListener('resize', refreshStatsOverlap)
    return () => {
      observer.disconnect()
      window.removeEventListener('resize', refreshStatsOverlap)
    }
  }, [activeId, refreshStatsOverlap])

  const onScroll = () => {
    const el = scrollRef.current
    if (!el) return
    nearBottom.current = el.scrollHeight - el.scrollTop - el.clientHeight < 80
    refreshStatsOverlap()
  }

  // 侧栏拖拽调宽
  const onResizeStart = (event: ReactPointerEvent<HTMLDivElement>) => {
    event.preventDefault()
    resizing.current = true
    resizeStartX.current = event.clientX
    resizeStartW.current = sidebarWidth
    document.body.classList.add('resizing')
    event.currentTarget.setPointerCapture(event.pointerId)
  }
  const onResizeMove = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (!resizing.current) return
    setSidebarWidth(clampSidebar(resizeStartW.current + event.clientX - resizeStartX.current))
  }
  const onResizeEnd = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (!resizing.current) return
    resizing.current = false
    document.body.classList.remove('resizing')
    event.currentTarget.releasePointerCapture(event.pointerId)
  }

  const submit = useCallback(
    async (text: string, files: FileAttachment[] = []) => {
      setError(null)
      try {
        if (!activeId) {
          const created = await api.createSession({
            model: draftModel ?? undefined,
            variant: draftVariant,
            search: draftSearch,
            workspace: draftProject || undefined,
          })
          setActiveId(created.session.id)
          await api.submit(created.session.id, text, files)
        } else {
          await api.submit(activeId, text, files)
        }
        setPending(text || null)
      } catch (e) {
        setError(toError(e))
      }
    },
    [activeId, draftModel, draftVariant, draftSearch, draftProject],
  )

  const abort = useCallback(async () => {
    if (!activeId) return
    try {
      await api.abort(activeId)
    } catch (e) {
      setError(toError(e))
    }
  }, [activeId])

  // 回退：丢弃该用户轮次及之后的内容，提示词放回输入框重新编辑
  const revertTurn = useCallback(
    async (turnIndex: number, prompt: string) => {
      if (!activeId) return
      try {
        await api.revert(activeId, turnIndex)
        setPending(null)
        composerRef.current?.set(prompt)
      } catch (e) {
        setError(toError(e))
      }
    },
    [activeId],
  )

  // 分叉：保留该用户轮次之前的历史创建新会话，并切换过去
  const forkTurn = useCallback(
    async (turnIndex: number) => {
      if (!activeId) return
      try {
        const created = await api.fork(activeId, turnIndex)
        setActiveId(created.session.id)
      } catch (e) {
        setError(toError(e))
      }
    },
    [activeId],
  )

  // 新建：仅预选项目并回到空态，会话延迟到首条消息发送时创建
  const newChat = useCallback((workspace?: string) => {
    setError(null)
    setDraftProject(workspace?.trim() ? workspace : '')
    setActiveId(null)
    setSidebarOpen(false)
  }, [])

  const removeSession = useCallback(
    async (id: string) => {
      try {
        await api.deleteSession(id)
        setSessions(prev => prev.filter(item => item.id !== id))
        if (activeId === id) setActiveId(null)
      } catch (e) {
        setError(toError(e))
      }
    },
    [activeId],
  )

  const configure = useCallback(
    async (body: ConfigureBody) => {
      if (!activeId) return
      try {
        await api.configure(activeId, body)
      } catch (e) {
        setError(toError(e))
      }
    },
    [activeId],
  )

  const currentModel = active?.current_model ?? draftModel ?? models?.default ?? ''
  const currentVariant = active?.current_variant ?? draftVariant
  const currentSearch = active?.web_search ?? draftSearch
  const currentEditor = active?.editor ?? 'replace'
  const modelEntry = models?.models.find(entry => entry.spec === currentModel)
  const workspaceName =
    active?.workspace_root.split('/').filter(Boolean).pop() ?? active?.workspace_root ?? null
  const contextLimit = stats?.context_limit ?? 200_000
  const contextPercent = stats
    ? Math.min(100, Math.round((stats.context_tokens / contextLimit) * 100))
    : 0

  const changeModel = (spec: string) => {
    if (activeId) {
      configure({ model: spec })
      return
    }
    setDraftModel(spec || null)
    const variants = models?.models.find(entry => entry.spec === spec)?.variants ?? []
    if (draftVariant && !variants.includes(draftVariant)) setDraftVariant(null)
  }

  const changeVariant = (variant: string | null) => {
    if (activeId) {
      configure({ variant })
      return
    }
    setDraftVariant(variant)
  }

  const changeSearch = (search: boolean) => {
    if (activeId) {
      configure({ search })
      return
    }
    setDraftSearch(search)
  }

  const modelOptions =
    models?.models.length && models.models.length > 0
      ? models.models
      : [{ spec: currentModel, name: currentModel, provider: '', variants: [] }]

  // 新建会话的项目候选：服务器默认目录优先，其余来自既有会话
  const projects = useMemo<ProjectOption[]>(() => {
    const map = new Map<string, string>()
    if (defaultRoot) {
      map.set(defaultRoot, projectName(defaultRoot))
    }
    for (const session of sessions) {
      if (!map.has(session.workspace_root)) {
        map.set(session.workspace_root, projectName(session.workspace_root))
      }
    }
    return [...map.entries()].map(([root, name]) => ({ root, name }))
  }, [sessions, defaultRoot])

  const composerControls = (
    <>
      <ChipSelect
        title="模型"
        value={currentModel}
        options={modelOptions.map(entry => ({ value: entry.spec, label: entry.name || entry.spec }))}
        onChange={changeModel}
      />
      {modelEntry && modelEntry.variants.length > 0 && (
        <ChipSelect
          title="推理档位"
          value={currentVariant ?? ''}
          options={[
            { value: '', label: 'default' },
            ...modelEntry.variants.map(variant => ({ value: variant, label: variant })),
          ]}
          onChange={variant => changeVariant(variant || null)}
        />
      )}
      <label className="chip-toggle" title="原生网络搜索">
        <input
          type="checkbox"
          checked={currentSearch}
          onChange={event => changeSearch(event.target.checked)}
        />
        <span className="toggle-dot" />
        搜索
      </label>
    </>
  )

  return (
    <div
      className={`app${sidebarOpen ? ' sidebar-open' : ''}${
        sidebarHidden ? ' sidebar-hidden' : ''
      }${fileSidebarOpen ? ' file-sidebar-open' : ''}`}
    >
      <Sidebar
        groups={groupByProject(sessions)}
        projects={projects}
        activeId={activeId}
        width={sidebarWidth}
        hiddenProjects={hiddenProjects}
        onHideProject={project =>
          setHiddenProjects(prev => new Set([...prev, project]))
        }
        onResizeStart={onResizeStart}
        onResizeMove={onResizeMove}
        onResizeEnd={onResizeEnd}
        onSelect={id => {
          setActiveId(id)
          setSidebarOpen(false)
        }}
        onCreate={newChat}
        onDelete={removeSession}
        onOpenSettings={() => setSettingsOpen(true)}
      />
      {sidebarOpen && <div className="backdrop" onClick={() => setSidebarOpen(false)} />}
      <main className="main">
        <header className="topbar">
          <button
            className="icon-ghost menu"
            onClick={() => {
              if (window.innerWidth <= 820) {
                setSidebarOpen(true)
              } else {
                setSidebarHidden(value => !value)
              }
            }}
            aria-label="会话列表"
            title="收起 / 展开会话列表"
          >
            {sidebarHidden ? (
              // 收起态：空面板 + 指向内容的箭头
              <svg
                viewBox="0 0 24 24"
                width="18"
                height="18"
                fill="none"
                stroke="currentColor"
                strokeWidth="1.8"
                strokeLinecap="round"
                strokeLinejoin="round"
                aria-hidden
              >
                <rect x="3" y="4.5" width="18" height="15" rx="2.5" />
                <path d="M9.5 4.5v15" />
                <path d="M6 9.5l2.5 2.5L6 14.5" />
              </svg>
            ) : (
              // 展开态：左侧面板填充
              <svg
                viewBox="0 0 24 24"
                width="18"
                height="18"
                fill="none"
                stroke="currentColor"
                strokeWidth="1.8"
                strokeLinecap="round"
                strokeLinejoin="round"
                aria-hidden
              >
                <rect x="3" y="4.5" width="18" height="15" rx="2.5" />
                <path d="M9.5 4.5v15" />
                <rect x="4.8" y="6.3" width="3.4" height="11.4" rx="1" fill="currentColor" stroke="none" />
              </svg>
            )}
          </button>
          <h1 className="topbar-title">{active?.title || '新对话'}</h1>
          {active && (
            <button
              className={`icon-ghost file-sidebar-toggle${fileSidebarOpen ? ' active' : ''}`}
              onClick={() => setFileSidebarOpen(value => !value)}
              title={fileSidebarOpen ? '收起相关文件' : '展开相关文件'}
              aria-label="相关文件预览"
            >
              <svg
                viewBox="0 0 24 24"
                width="18"
                height="18"
                fill="none"
                stroke="currentColor"
                strokeWidth="1.8"
                strokeLinecap="round"
                strokeLinejoin="round"
                aria-hidden
              >
                <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
                <polyline points="14 2 14 8 20 8" />
                <line x1="16" y1="13" x2="8" y2="13" />
                <line x1="16" y1="17" x2="8" y2="17" />
                <polyline points="10 9 9 9 8 9" />
              </svg>
              {relatedFiles.length > 0 && (
                <span className="file-sidebar-badge">{relatedFiles.length}</span>
              )}
            </button>
          )}
          {stats && active && (
            <button
              className={`icon-ghost stats-toggle${statsVisible ? ' active' : ''}`}
              onClick={() => {
                if (statsVisible) {
                  setStatsCollapsed(true)
                  setStatsPinned(false)
                } else {
                  setStatsCollapsed(false)
                  // 遮挡位置强制显示，保持到滚动至不再遮挡
                  setStatsPinned(true)
                }
              }}
              title={statsVisible ? '收起用量面板' : '展开用量面板'}
              aria-label="用量面板"
            >
              <svg
                viewBox="0 0 24 24"
                width="18"
                height="18"
                fill="none"
                stroke="currentColor"
                strokeWidth="1.8"
                strokeLinecap="round"
                strokeLinejoin="round"
                aria-hidden
              >
                <polyline points="22 12 18 12 15 21 9 3 6 12 2 12" />
              </svg>
            </button>
          )}
        </header>
        {stats && active && (
          <div
            className={`stats-panel${statsVisible ? '' : ' hidden'}`}
            ref={statsPanelRef}
          >
            <div className="stat-row">
              <span className="stat-label">Session</span>
              <span className="stat-value">{active.id}</span>
            </div>
            <div className="stat-row">
              <span className="stat-label">Workspace</span>
              <span className="stat-value">{workspaceName}</span>
            </div>
            <div className="stat-row">
              <span className="stat-label">Editor</span>
              <span className="stat-value">{currentEditor}</span>
            </div>
            <div className="stat-row">
              <span className="stat-label">Turns</span>
              <span className="stat-value">{fmtNum(stats.total_turns)}</span>
            </div>
            <div className="stat-row">
              <span className="stat-label">Context</span>
              <span className="stat-value">
                {fmtNum(stats.context_tokens)} / {fmtLimit(contextLimit)}
              </span>
            </div>
            <div className="ctx-gauge">
              <span className="ctx-gauge-track">
                <span className="ctx-gauge-fill" style={{ width: `${contextPercent}%` }} />
              </span>
              <span>{contextPercent}%</span>
            </div>
            <div className="stat-row">
              <span className="stat-label">In</span>
              <span className="stat-value">{fmtNum(stats.prompt_tokens)}</span>
            </div>
            <div className="stat-row">
              <span className="stat-label">Out</span>
              <span className="stat-value">{fmtNum(stats.completion_tokens)}</span>
            </div>
            {stats.tps != null && (
              <div className="stat-row">
                <span className="stat-label">TPS</span>
                <span className="stat-value">{stats.tps.toFixed(1)}</span>
              </div>
            )}
            <div className="stat-row">
              <span className="stat-label">Cache Read</span>
              <span className="stat-value">{fmtNum(stats.cache_read_tokens)}</span>
            </div>
            <div className="stat-row">
              <span className="stat-label">Cache Write</span>
              <span className="stat-value">{fmtNum(stats.cache_write_tokens)}</span>
            </div>
            <div className="stat-row">
              <span className="stat-label">Est. Cost</span>
              <span className="stat-value">${stats.estimated_cost_usd.toFixed(4)}</span>
            </div>
          </div>
        )}
        {error && (
          <div className="banner error" onClick={() => setError(null)}>
            {error}
          </div>
        )}
        {snap && snap.notices.length > 0 && (
          <div className="banner notice">
            {snap.notices.map((notice, index) => (
              <div key={index}>{notice}</div>
            ))}
          </div>
        )}
        {!activeId ? (
          <div className="empty-stage">
            <div className="empty-brand">vva</div>
            <div className="empty-hint">选择项目，输入任务开始 · Enter 发送</div>
            <div className="empty-composer">
              <div className="empty-project">
                <ChipSelect
                  title="项目"
                  value={draftProject || defaultRoot || ''}
                  options={projects.map(project => ({ value: project.root, label: project.name }))}
                  onChange={setDraftProject}
                />
              </div>
              <Composer running={running} onSend={submit} onStop={abort} controls={composerControls} />
            </div>
          </div>
        ) : (
          <>
            <div className="chat-scroll" ref={scrollRef} onScroll={onScroll}>
              <Chat
                snap={snap}
                pending={pending}
                onRevert={revertTurn}
                onFork={forkTurn}
                onPreviewFile={onPreviewFile}
              />
            </div>
            <div className="composer-area">
              <Composer
                ref={composerRef}
                running={running}
                onSend={submit}
                onStop={abort}
                controls={composerControls}
              />
            </div>
          </>
        )}
      </main>
      {active && (
        <>
          <FileSidebar
            sessionId={activeId}
            session={active}
            draft={snap?.draft}
            activePath={previewFilePath}
            open={fileSidebarOpen}
            width={fileSidebarWidth}
            onSelectPath={setPreviewFilePath}
            onClose={() => setFileSidebarOpen(false)}
            onResizeStart={onFileResizeStart}
            onResizeMove={onFileResizeMove}
            onResizeEnd={onFileResizeEnd}
          />
          {fileSidebarOpen && (
            <div className="file-backdrop" onClick={() => setFileSidebarOpen(false)} />
          )}
        </>
      )}
      {settingsOpen && (
        <SettingsModal
          onClose={() => setSettingsOpen(false)}
          onSaved={() => {
            api.models().then(setModels).catch(() => {})
          }}
        />
      )}
    </div>
  )
}
