import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type { PointerEvent as ReactPointerEvent } from 'react'
import { api } from './api'
import type { ConfigureBody } from './api'
import { Chat, turnText } from './components/Chat'
import { ChipSelect } from './components/ChipSelect'
import { Composer, type ComposerHandle } from './components/Composer'
import { Sidebar, groupByProject, projectName } from './components/Sidebar'
import type { ProjectOption } from './components/Sidebar'
import type { ImageAttachment, ModelsResponse, SessionSnapshot, SessionSummary, SidebarStats } from './types'

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
  const resizing = useRef(false)
  const resizeStartX = useRef(0)
  const resizeStartW = useRef(0)
  const composerRef = useRef<ComposerHandle>(null)

  useEffect(() => {
    localStorage.setItem('vva.sidebar-hidden', sidebarHidden ? '1' : '0')
  }, [sidebarHidden])

  useEffect(() => {
    localStorage.setItem('vva.projects-hidden', JSON.stringify([...hiddenProjects]))
  }, [hiddenProjects])

  useEffect(() => {
    localStorage.setItem('vva.sidebar-width', String(sidebarWidth))
  }, [sidebarWidth])

  // 未创建会话前的模型偏好，创建会话时生效
  const [draftModel, setDraftModel] = useState<string | null>(null)
  const [draftVariant, setDraftVariant] = useState<string | null>(null)
  const [draftSearch, setDraftSearch] = useState(true)
  const [draftProject, setDraftProject] = useState('')

  const running = snap !== null && snap.state !== ''
  const active = snap?.session ?? null
  const turnCount = active?.turns.length ?? 0

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
      return
    }
    setSnap(null)
    setPending(null)
    setStats(null)
    const source = new EventSource(`/api/sessions/${activeId}/events`)
    source.onmessage = event => {
      try {
        setSnap(JSON.parse(event.data) as SessionSnapshot)
      } catch {
        // 忽略无法解析的帧
      }
    }
    return () => source.close()
  }, [activeId])

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

  const onScroll = () => {
    const el = scrollRef.current
    if (!el) return
    nearBottom.current = el.scrollHeight - el.scrollTop - el.clientHeight < 80
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
    async (text: string, images: ImageAttachment[] = []) => {
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
          await api.submit(created.session.id, text, images)
        } else {
          await api.submit(activeId, text, images)
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

  const newChat = useCallback(
    async (workspace?: string) => {
      setError(null)
      try {
        const created = await api.createSession({
          model: draftModel ?? undefined,
          variant: draftVariant,
          search: draftSearch,
          workspace: workspace?.trim() ? workspace : undefined,
        })
        setActiveId(created.session.id)
        setSidebarOpen(false)
      } catch (e) {
        setError(toError(e))
      }
    },
    [draftModel, draftVariant, draftSearch],
  )

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
        setSnap(await api.configure(activeId, body))
      } catch (e) {
        setError(toError(e))
      }
    },
    [activeId],
  )

  const currentModel = active?.current_model ?? draftModel ?? models?.default ?? ''
  const currentVariant = active?.current_variant ?? draftVariant
  const currentSearch = active?.web_search ?? draftSearch
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
      {!activeId && (
        <ChipSelect
          title="项目"
          value={draftProject || defaultRoot || ''}
          options={projects.map(project => ({ value: project.root, label: project.name }))}
          onChange={setDraftProject}
        />
      )}
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
      }`}
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
        </header>
        {stats && active && (
          <div className="stats-panel">
            <div className="stat-row">
              <span className="stat-label">Session</span>
              <span className="stat-value">{active.id}</span>
            </div>
            <div className="stat-row">
              <span className="stat-label">Workspace</span>
              <span className="stat-value">{workspaceName}</span>
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
            <Composer running={running} onSend={submit} onStop={abort} controls={composerControls} />
          </div>
        ) : (
          <>
            <div className="chat-scroll" ref={scrollRef} onScroll={onScroll}>
              <Chat snap={snap} pending={pending} onRevert={revertTurn} onFork={forkTurn} />
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
    </div>
  )
}
