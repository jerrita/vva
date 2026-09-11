import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type { PointerEvent as ReactPointerEvent } from 'react'
import { api } from './api'
import type { ConfigureBody } from './api'
import { Chat, turnText } from './components/Chat'
import { Composer } from './components/Composer'
import { Sidebar, groupByProject, projectName } from './components/Sidebar'
import type { ProjectOption } from './components/Sidebar'
import type { ModelsResponse, SessionSnapshot, SessionSummary, SidebarStats } from './types'

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

function fmtCost(value: number): string {
  if (value <= 0) return '—'
  return `$${value >= 0.01 ? value.toFixed(2) : value.toFixed(4)}`
}

function StatRow({ label, value, sub }: { label: string; value: string; sub?: string }) {
  return (
    <div className="stat-row">
      <span className="stat-label">{label}</span>
      <span className="stat-value">
        {value}
        {sub && <span className="stat-sub">{sub}</span>}
      </span>
    </div>
  )
}

export default function App() {
  const [models, setModels] = useState<ModelsResponse | null>(null)
  const [sessions, setSessions] = useState<SessionSummary[]>([])
  const [activeId, setActiveId] = useState<string | null>(null)
  const [snap, setSnap] = useState<SessionSnapshot | null>(null)
  const [pending, setPending] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [sidebarOpen, setSidebarOpen] = useState(false)
  const [statsOpen, setStatsOpen] = useState(false)
  const [stats, setStats] = useState<SidebarStats | null>(null)
  const [defaultRoot, setDefaultRoot] = useState<string | null>(null)
  const [sidebarHidden, setSidebarHidden] = useState(
    () => localStorage.getItem('vva.sidebar-hidden') === '1',
  )
  const [sidebarWidth, setSidebarWidth] = useState(() => {
    const stored = Number(localStorage.getItem('vva.sidebar-width'))
    return stored >= SIDEBAR_MIN && stored <= SIDEBAR_MAX ? stored : 268
  })
  const resizing = useRef(false)
  const resizeStartX = useRef(0)
  const resizeStartW = useRef(0)

  useEffect(() => {
    localStorage.setItem('vva.sidebar-hidden', sidebarHidden ? '1' : '0')
  }, [sidebarHidden])

  useEffect(() => {
    localStorage.setItem('vva.sidebar-width', String(sidebarWidth))
  }, [sidebarWidth])

  // 未创建会话前的模型偏好，创建会话时生效
  const [draftModel, setDraftModel] = useState<string | null>(null)
  const [draftVariant, setDraftVariant] = useState<string | null>(null)
  const [draftSearch, setDraftSearch] = useState(true)

  const running = snap !== null && snap.state !== ''
  const active = snap?.session ?? null
  const turnCount = active?.turns.length ?? 0

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
    setStatsOpen(false)
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

  // 统计面板数据：打开时与轮次变化时刷新
  useEffect(() => {
    if (!statsOpen || !activeId) return
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
  }, [statsOpen, activeId, turnCount])

  const onScroll = () => {
    const el = scrollRef.current
    if (!el) return
    nearBottom.current = el.scrollHeight - el.scrollTop - el.clientHeight < 80
  }

  // Esc 关闭统计浮块；主界面点击不关闭
  useEffect(() => {
    if (!statsOpen) return
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setStatsOpen(false)
    }
    document.addEventListener('keydown', onKeyDown)
    return () => document.removeEventListener('keydown', onKeyDown)
  }, [statsOpen])

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
    async (text: string) => {
      setError(null)
      try {
        if (!activeId) {
          const created = await api.createSession({
            model: draftModel ?? undefined,
            variant: draftVariant,
            search: draftSearch,
          })
          setActiveId(created.session.id)
          await api.submit(created.session.id, text)
        } else {
          await api.submit(activeId, text)
        }
        setPending(text)
      } catch (e) {
        setError(toError(e))
      }
    },
    [activeId, draftModel, draftVariant, draftSearch],
  )

  const abort = useCallback(async () => {
    if (!activeId) return
    try {
      await api.abort(activeId)
    } catch (e) {
      setError(toError(e))
    }
  }, [activeId])

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
      <select
        className="chip"
        value={currentModel}
        onChange={event => changeModel(event.target.value)}
        title="模型"
      >
        {modelOptions.map(entry => (
          <option key={entry.spec} value={entry.spec}>
            {entry.name || entry.spec}
          </option>
        ))}
      </select>
      {modelEntry && modelEntry.variants.length > 0 && (
        <select
          className="chip"
          value={currentVariant ?? ''}
          onChange={event => changeVariant(event.target.value || null)}
          title="推理档位"
        >
          <option value="">档位</option>
          {modelEntry.variants.map(variant => (
            <option key={variant} value={variant}>
              {variant}
            </option>
          ))}
        </select>
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
            <svg
              viewBox="0 0 24 24"
              width="18"
              height="18"
              fill="none"
              stroke="currentColor"
              strokeWidth="2"
              strokeLinecap="round"
              aria-hidden
            >
              <path d="M4 7h16M4 12h16M4 17h16" />
            </svg>
          </button>
          <h1 className="topbar-title">{active?.title || '新对话'}</h1>
          <button
            className="icon-ghost"
            onClick={() => setStatsOpen(value => !value)}
            disabled={!activeId}
            title="会话统计"
            aria-label="会话统计"
          >
            <svg
              viewBox="0 0 24 24"
              width="17"
              height="17"
              fill="none"
              stroke="currentColor"
              strokeWidth="2"
              strokeLinecap="round"
              aria-hidden
            >
              <path d="M5 20v-6M12 20V8M19 20V4" />
            </svg>
          </button>
        </header>
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
        {statsOpen && active && (
          <div className="stats-panel">
            <div className="stats-head">
              <span>会话统计</span>
              <span className="stats-model" title={active.current_model}>
                {active.current_model}
              </span>
            </div>
            {stats ? (
              <div className="stats-rows">
                <StatRow label="轮次" value={fmtNum(stats.total_turns)} />
                <StatRow
                  label="Token"
                  value={fmtNum(stats.total_tokens)}
                  sub={` ↑${fmtNum(stats.prompt_tokens)} ↓${fmtNum(stats.completion_tokens)}`}
                />
                <StatRow
                  label="上下文"
                  value={
                    stats.context_limit
                      ? `${Math.min(100, Math.round((stats.context_tokens / stats.context_limit) * 100))}%`
                      : fmtNum(stats.context_tokens)
                  }
                  sub={
                    stats.context_limit
                      ? ` ${fmtNum(stats.context_tokens)} / ${fmtNum(stats.context_limit)}`
                      : undefined
                  }
                />
                <StatRow
                  label="缓存"
                  value={`读 ${fmtNum(stats.cache_read_tokens)}`}
                  sub={` · 写 ${fmtNum(stats.cache_write_tokens)}`}
                />
                <StatRow label="预估费用" value={fmtCost(stats.estimated_cost_usd)} />
              </div>
            ) : (
              <div className="stats-empty">加载中…</div>
            )}
          </div>
        )}
        <div className="chat-scroll" ref={scrollRef} onScroll={onScroll}>
          <Chat snap={snap} pending={pending} />
        </div>
        <div className="composer-area">
          <Composer running={running} onSend={submit} onStop={abort} controls={composerControls} />
        </div>
      </main>
    </div>
  )
}
