import { useCallback, useEffect, useMemo, useRef, useState, type CSSProperties } from 'react'
import { api, toError } from './api'
import { subscribeSession } from './sessionSync'
import type { ConfigureBody } from './api'
import { Chat, turnText } from './components/Chat'
import { AskUserModal } from './components/AskUserModal'
import { ChipSelect } from './components/ChipSelect'
import { Composer, type ComposerHandle } from './components/Composer'
import { EmptyStage } from './components/EmptyStage'
import { Sash } from './components/Sash'
import { Sidebar, groupByProject, projectName, type ProjectOption } from './components/Sidebar'
import { SettingsModal } from './components/SettingsModal'
import { StatsPanel } from './components/StatsPanel'
import { IconActivity, IconPanelLeft, IconPanelRight } from './components/icons'
import { FileSidebar } from './components/FileSidebar'
import { useComposerVisibility } from './hooks/useComposerVisibility'
import { usePanelLayout } from './hooks/usePanelLayout'
import { useStatsPanel } from './hooks/useStatsPanel'
import { usePersistentState } from './hooks/usePersistentState'
import { extractRelatedFiles } from './utils/relatedFiles'
import type { FileAttachment, ModelsResponse, SessionSnapshot, SessionSummary, SkillEntry, UserAnswer } from './types'

const ACTIVE_SESSION_STORAGE_KEY = 'vva.active-session'

export default function App() {
  const [models, setModels] = useState<ModelsResponse | null>(null)
  const [sessions, setSessions] = useState<SessionSummary[]>([])
  const [activeId, setActiveId] = useState<string | null>(null)
  const [sessionsLoaded, setSessionsLoaded] = useState(false)
  const [snap, setSnap] = useState<SessionSnapshot | null>(null)
  const [pending, setPending] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [sidebarOpen, setSidebarOpen] = useState(false)
  const [defaultRoot, setDefaultRoot] = useState<string | null>(null)
  const [skills, setSkills] = useState<SkillEntry[]>([])
  const [hiddenProjects, setHiddenProjects] = usePersistentState<Set<string>>(
    'vva.projects-hidden',
    () => new Set(JSON.parse(localStorage.getItem('vva.projects-hidden') ?? '[]') as string[]),
    value => JSON.stringify([...value]),
  )
  const [projectOrder, setProjectOrder] = usePersistentState<string[]>(
    'vva.projects-order',
    () => {
      try {
        const value = JSON.parse(localStorage.getItem('vva.projects-order') ?? '[]')
        return Array.isArray(value) && value.every(item => typeof item === 'string') ? value : []
      } catch {
        return []
      }
    },
    value => JSON.stringify(value),
  )
  const [unreadIds, setUnreadIds] = usePersistentState<Set<string>>(
    'vva.sessions-unread',
    () => {
      try {
        const value = JSON.parse(localStorage.getItem('vva.sessions-unread') ?? '[]')
        return new Set(Array.isArray(value) ? value.filter(item => typeof item === 'string') : [])
      } catch {
        return new Set()
      }
    },
    value => JSON.stringify([...value]),
  )
  const [settingsOpen, setSettingsOpen] = useState(false)
  const composerRef = useRef<ComposerHandle>(null)

  const {
    sidebarHidden,
    setSidebarHidden,
    sidebarWidth,
    sidebarResize,
    fileSidebarOpen,
    setFileSidebarOpen,
    fileSidebarWidth,
    fileSidebarResize,
    previewFilePath,
    setPreviewFilePath,
    onPreviewFile,
  } = usePanelLayout()

  // 未创建会话前的模型偏好，创建会话时生效
  const [draftModel, setDraftModel] = useState<string | null>(null)
  const [draftVariant, setDraftVariant] = useState<string | null>(null)
  const [draftSearch, setDraftSearch] = useState(true)
  const [draftProject, setDraftProject] = useState('')

  const running = snap !== null && snap.state !== ''
  // AskUser 提问挂起：面板停靠输入框上方，输入区保持展示可交互
  const askPending = snap?.pending_user_question != null
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
  }, [active?.id, active?.workspace_root, setHiddenProjects])

  const scrollRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    api.models().then(setModels).catch(e => setError(toError(e)))
    api.workspace().then(workspace => setDefaultRoot(workspace.root)).catch(() => {})
    api
      .sessions()
      .then(list => {
        setSessions(list)
        // 会话链接 /s/<id> 优先于本地上次会话
        const urlSessionId = /^\/s\/([^/]+)$/.exec(location.pathname)?.[1] ?? null
        const storedActiveId = localStorage.getItem(ACTIVE_SESSION_STORAGE_KEY)
        const initialId = [urlSessionId, storedActiveId].find(
          id => id != null && list.some(session => session.id === id),
        )
        if (initialId) {
          setActiveId(initialId)
        } else if (storedActiveId) {
          localStorage.removeItem(ACTIVE_SESSION_STORAGE_KEY)
        }
        setSessionsLoaded(true)
      })
      .catch(e => setError(toError(e)))
    api.skills().then(response => setSkills(response.skills)).catch(() => {})
  }, [])

  // 当前会话持久化到 localStorage，并同步地址栏 /s/<id>（链接直达，无路由）
  useEffect(() => {
    if (!sessionsLoaded) return
    if (activeId) {
      localStorage.setItem(ACTIVE_SESSION_STORAGE_KEY, activeId)
    } else {
      localStorage.removeItem(ACTIVE_SESSION_STORAGE_KEY)
    }
    history.replaceState(null, '', activeId ? `/s/${activeId}` : '/')
  }, [activeId, sessionsLoaded])

  useEffect(() => {
    const roots = new Set(sessions.map(session => session.workspace_root))
    setProjectOrder(prev => {
      const next = [...prev]
      for (const root of roots) {
        if (!next.includes(root)) next.push(root)
      }
      return next.length === prev.length ? prev : next
    })
  }, [sessions, setProjectOrder])

  useEffect(() => {
    const summary = sessions.find(session => session.id === activeId)
    document.title = active?.title?.trim() || summary?.title?.trim() || 'vva'
  }, [active, activeId, sessions])

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
    setPreviewFilePath(null)
    return subscribeSession(activeId, setSnap, e => setError(toError(e)))
  }, [activeId])

  const relatedFiles = useMemo(
    () => (active ? extractRelatedFiles(active, snap?.draft) : []),
    [active, snap?.draft],
  )

  // 会话在本地会话列表中保持同步（标题 / 模型 / 轮次 / 运行态）
  const listKey = useRef('')
  useEffect(() => {
    if (!snap || !snap.persisted) return
    const s = snap.session
    const key = `${s.id}|${s.title ?? ''}|${s.current_model}|${s.current_variant ?? ''}|${s.turns.length}|${snap.state}`
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
      state: snap.state,
    }
    setSessions(prev => [summary, ...prev.filter(item => item.id !== s.id)])
  }, [snap])

  const activeIdRef = useRef(activeId)
  activeIdRef.current = activeId
  const runningWatchKey = sessions
    .filter(session => session.id !== activeId && Boolean(session.state))
    .map(session => session.id)
    .sort()
    .join('|')

  // 后台生成：对非当前会话保持轻量 SSE，结束后标未读
  useEffect(() => {
    if (!runningWatchKey) return
    const unsubscribers = runningWatchKey.split('|').map(id =>
      subscribeSession(
        id,
        next => {
          setSessions(prev =>
            prev.map(item =>
              item.id === next.session.id
                ? {
                    ...item,
                    title: next.session.title ?? item.title,
                    turn_count: next.session.turns.length,
                    current_model: next.session.current_model,
                    current_variant: next.session.current_variant ?? null,
                    web_search: next.session.web_search,
                    state: next.state,
                  }
                : item,
            ),
          )
          if (next.state === '' && activeIdRef.current !== next.session.id) {
            setUnreadIds(prev => {
              if (prev.has(next.session.id)) return prev
              const nextSet = new Set(prev)
              nextSet.add(next.session.id)
              return nextSet
            })
          }
        },
        () => {},
      ),
    )
    return () => {
      for (const unsubscribe of unsubscribers) unsubscribe()
    }
  }, [runningWatchKey, setUnreadIds])

  const prevActiveRef = useRef<{ id: string | null; state: string }>({
    id: null,
    state: '',
  })
  useEffect(() => {
    const previous = prevActiveRef.current
    if (previous.id && previous.id !== activeId && previous.state) {
      setSessions(prev =>
        prev.map(item =>
          item.id === previous.id && !item.state ? { ...item, state: previous.state } : item,
        ),
      )
    }
    prevActiveRef.current = { id: activeId, state: snap?.state ?? '' }
  }, [activeId, snap?.state])

  useEffect(() => {
    if (!activeId) return
    setUnreadIds(prev => {
      if (!prev.has(activeId)) return prev
      const next = new Set(prev)
      next.delete(activeId)
      return next
    })
  }, [activeId, setUnreadIds])

  useEffect(() => {
    const ids = new Set(sessions.map(session => session.id))
    setUnreadIds(prev => {
      let changed = false
      const next = new Set<string>()
      for (const id of prev) {
        if (ids.has(id)) next.add(id)
        else changed = true
      }
      return changed ? next : prev
    })
  }, [sessions, setUnreadIds])

  // 乐观展示的用户消息在会话中出现后移除
  useEffect(() => {
    if (!pending || !snap) return
    const exists = snap.session.turns.some(
      turn => turn.role === 'user' && turnText(turn) === pending,
    )
    if (exists) setPending(null)
  }, [snap, pending])

  const { stats, statsVisible, statsPanelRef, toggle: toggleStats, refreshStatsOverlap } =
    useStatsPanel({
      activeId,
      turnCount,
      contentKey: snap,
      scrollRef,
    })
  const {
    scrollProps,
    composerAreaRef,
    composerAreaProps,
    composerHidden,
    topbarHidden,
    revealComposer,
    followBottom,
  } = useComposerVisibility({ activeId, scrollRef, onScrollExtra: refreshStatsOverlap })

  // 用户位于底部时跟随滚动
  useEffect(() => {
    followBottom()
  }, [snap, followBottom])

  const submit = useCallback(
    async (text: string, files: FileAttachment[] = []) => {
      setError(null)
      try {
        let id = activeId
        if (!id) {
          const created = await api.createSession({
            model: draftModel ?? undefined,
            variant: draftVariant,
            search: draftSearch,
            workspace: draftProject || undefined,
          })
          id = created.session.id
          setActiveId(id)
        }
        await api.submit(id, text, files)
        setPending(text || null)
        setSessions(prev =>
          prev.map(item => (item.id === id ? { ...item, state: 'Thinking...' } : item)),
        )
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

  const answerUser = useCallback(
    async (answers: UserAnswer[]) => {
      if (!activeId || !snap?.pending_user_question) return
      try {
        await api.answerUser(activeId, snap.pending_user_question.id, answers)
      } catch (e) {
        setError(toError(e))
      }
    },
    [activeId, snap?.pending_user_question],
  )

  // 回退：丢弃该用户轮次及之后的内容，提示词放回输入框重新编辑
  const revertTurn = useCallback(
    async (turnIndex: number, prompt: string) => {
      if (!activeId) return
      revealComposer()
      try {
        await api.revert(activeId, turnIndex)
        setPending(null)
        composerRef.current?.set(prompt)
      } catch (e) {
        setError(toError(e))
      }
    },
    [activeId, revealComposer],
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

  const reorderProjects = useCallback(
    (sourceRoot: string, targetRoot: string) => {
      setProjectOrder(prev => {
        const sourceIndex = prev.indexOf(sourceRoot)
        const targetIndex = prev.indexOf(targetRoot)
        if (sourceIndex < 0 || targetIndex < 0 || sourceIndex === targetIndex) return prev
        const next = [...prev]
        next.splice(sourceIndex, 1)
        next.splice(next.indexOf(targetRoot), 0, sourceRoot)
        return next
      })
    },
    [setProjectOrder],
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
    (models?.models.length ?? 0) > 0
      ? models!.models
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
      style={{ '--sidebar-width': `${sidebarWidth}px`, '--file-sidebar-width': `${fileSidebarWidth}px` } as CSSProperties}
    >
      <Sidebar
        groups={groupByProject(sessions, projectOrder)}
        projects={projects}
        activeId={activeId}
        unreadIds={unreadIds}
        width={sidebarWidth}
        skills={skills}
        hiddenProjects={hiddenProjects}
        onHideProject={project =>
          setHiddenProjects(prev => new Set([...prev, project]))
        }
        onReorder={reorderProjects}
        onSelect={id => {
          setActiveId(id)
          setSidebarOpen(false)
        }}
        onCreate={newChat}
        onDelete={removeSession}
        onOpenSettings={() => setSettingsOpen(true)}
      />
      <Sash
        className="sash-sidebar"
        label="拖拽调节会话列表宽度"
        onStart={sidebarResize.onStart}
        onMove={sidebarResize.onMove}
        onEnd={sidebarResize.onEnd}
      />
      <div className="backdrop" onClick={() => setSidebarOpen(false)} />
      <main className={`main${topbarHidden ? ' topbar-hidden' : ''}`}>
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
            <IconPanelLeft collapsed={sidebarHidden} size={18} />
          </button>
          <h1 className="topbar-title">{active?.title || '新对话'}</h1>
          {stats && active && (
            <button
              className={`icon-ghost stats-toggle${statsVisible ? ' active' : ''}`}
              onClick={toggleStats}
              title={statsVisible ? '收起用量面板' : '展开用量面板'}
              aria-label="用量面板"
            >
              <IconActivity size={18} />
            </button>
          )}
          {active && (
            <button
              className={`icon-ghost file-sidebar-toggle${fileSidebarOpen ? ' active' : ''}`}
              onClick={() => setFileSidebarOpen(value => !value)}
              title={fileSidebarOpen ? '收起文件浏览器' : '展开文件浏览器'}
              aria-label="工程文件浏览器"
            >
              <IconPanelRight size={18} />
              {relatedFiles.length > 0 && (
                <span className="file-sidebar-badge">{relatedFiles.length}</span>
              )}
            </button>
          )}
        </header>
        {stats && active && (
          <StatsPanel stats={stats} active={active} visible={statsVisible} panelRef={statsPanelRef} />
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
          <EmptyStage
            projects={projects}
            defaultRoot={defaultRoot}
            draftProject={draftProject}
            onDraftProjectChange={setDraftProject}
            controls={composerControls}
            running={running}
            skills={skills}
            onSend={submit}
            onStop={abort}
          />
        ) : (
          <>
            <div className="chat-scroll" ref={scrollRef} {...scrollProps}>
              <Chat
                snap={snap}
                pending={pending}
                onRevert={revertTurn}
                onFork={forkTurn}
                onPreviewFile={onPreviewFile}
              />
            </div>
            <div
              className={`composer-area${composerHidden && !askPending ? ' composer-hidden' : ''}`}
              ref={composerAreaRef}
              {...composerAreaProps}
              aria-hidden={composerHidden && !askPending}
              inert={composerHidden && !askPending}
            >
              {/* AskUser 面板停靠输入框上方：无遮罩，聊天记录保持可见 */}
              {snap?.pending_user_question && (
                <AskUserModal
                  request={snap.pending_user_question}
                  onSubmit={answers => void answerUser(answers)}
                  onCancel={() => void answerUser([])}
                />
              )}
              <Composer
                ref={composerRef}
                running={running}
                onSend={submit}
                onStop={abort}
                controls={composerControls}
                skills={skills}
                workspace={active?.workspace_root}
              />
            </div>
          </>
        )}
      </main>
      {active && (
        <>
            <Sash
              className="sash-files"
              label="拖拽调节文件浏览器宽度"
              onStart={fileSidebarResize.onStart}
              onMove={fileSidebarResize.onMove}
              onEnd={fileSidebarResize.onEnd}
            />
          <FileSidebar
            sessionId={activeId}
            workspace={active.workspace_root}
            files={relatedFiles}
            activePath={previewFilePath}
            open={fileSidebarOpen}
            width={fileSidebarWidth}
            onSelectPath={setPreviewFilePath}
            onClose={() => setFileSidebarOpen(false)}
          />
          <div className="file-backdrop" onClick={() => setFileSidebarOpen(false)} />
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
