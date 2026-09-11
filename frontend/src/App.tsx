import { useCallback, useEffect, useRef, useState } from 'react'
import { api } from './api'
import type { ConfigureBody } from './api'
import { Chat, turnText } from './components/Chat'
import { Composer } from './components/Composer'
import { Sidebar } from './components/Sidebar'
import type { ModelsResponse, SessionSnapshot, SessionSummary } from './types'

function sorted(list: SessionSummary[]): SessionSummary[] {
  return [...list].sort((a, b) => b.created_at.localeCompare(a.created_at))
}

function toError(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

export default function App() {
  const [models, setModels] = useState<ModelsResponse | null>(null)
  const [sessions, setSessions] = useState<SessionSummary[]>([])
  const [activeId, setActiveId] = useState<string | null>(null)
  const [snap, setSnap] = useState<SessionSnapshot | null>(null)
  const [pending, setPending] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [sidebarOpen, setSidebarOpen] = useState(false)

  const running = snap !== null && snap.state !== ''
  const active = snap?.session ?? null

  const scrollRef = useRef<HTMLDivElement>(null)
  const nearBottom = useRef(true)

  useEffect(() => {
    api.models().then(setModels).catch(e => setError(toError(e)))
    api.sessions().then(list => setSessions(sorted(list))).catch(e => setError(toError(e)))
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
    setSessions(prev => sorted([summary, ...prev.filter(item => item.id !== s.id)]))
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

  const onScroll = () => {
    const el = scrollRef.current
    if (!el) return
    nearBottom.current = el.scrollHeight - el.scrollTop - el.clientHeight < 80
  }

  const submit = useCallback(
    async (text: string) => {
      setError(null)
      try {
        if (!activeId) {
          const created = await api.createSession({})
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
    [activeId],
  )

  const abort = useCallback(async () => {
    if (!activeId) return
    try {
      await api.abort(activeId)
    } catch (e) {
      setError(toError(e))
    }
  }, [activeId])

  const newChat = useCallback(async () => {
    setError(null)
    try {
      const created = await api.createSession({})
      setActiveId(created.session.id)
      setSidebarOpen(false)
    } catch (e) {
      setError(toError(e))
    }
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
        setSnap(await api.configure(activeId, body))
      } catch (e) {
        setError(toError(e))
      }
    },
    [activeId],
  )

  const currentModel = active?.current_model || models?.default || ''
  const modelEntry = models?.models.find(entry => entry.spec === currentModel)

  return (
    <div className={`app${sidebarOpen ? ' sidebar-open' : ''}`}>
      <Sidebar
        sessions={sessions}
        activeId={activeId}
        onSelect={id => {
          setActiveId(id)
          setSidebarOpen(false)
        }}
        onNew={newChat}
        onDelete={removeSession}
      />
      {sidebarOpen && <div className="backdrop" onClick={() => setSidebarOpen(false)} />}
      <main className="main">
        <header className="topbar">
          <button
            className="icon-ghost menu"
            onClick={() => setSidebarOpen(value => !value)}
            aria-label="会话列表"
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
          <div className="topbar-controls">
            <select
              value={currentModel}
              disabled={!activeId}
              onChange={event => configure({ model: event.target.value })}
              title="模型"
            >
              {(models?.models.length
                ? models.models
                : [{ spec: currentModel, name: currentModel, provider: '', variants: [] }]
              ).map(entry => (
                <option key={entry.spec} value={entry.spec}>
                  {entry.name || entry.spec}
                </option>
              ))}
            </select>
            {modelEntry && modelEntry.variants.length > 0 && (
              <select
                value={active?.current_variant ?? ''}
                disabled={!activeId}
                onChange={event => configure({ variant: event.target.value || null })}
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
            <label className="search-toggle" title="原生网络搜索">
              <input
                type="checkbox"
                checked={active?.web_search ?? false}
                disabled={!activeId}
                onChange={event => configure({ search: event.target.checked })}
              />
              搜索
            </label>
          </div>
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
        <div className="chat-scroll" ref={scrollRef} onScroll={onScroll}>
          <Chat snap={snap} pending={pending} />
        </div>
        <div className="composer-area">
          <Composer running={running} onSend={submit} onStop={abort} />
        </div>
      </main>
    </div>
  )
}
