import { useCallback, useEffect, useMemo, useRef, useState, type CSSProperties } from 'react'
import { api, toError } from './api'
import { sessionSummary, subscribeSession } from './sessionSync'
import type { ConfigureBody } from './api'
import { Chat, turnText } from './components/Chat'
import { AskUserModal } from './components/AskUserModal'
import { ChipSelect } from './components/ChipSelect'
import { Composer, type ComposerHandle } from './components/Composer'
import { EmptyStage } from './components/EmptyStage'
import { Sash } from './components/Sash'
import { Sidebar, groupByProject, projectName, type ProjectOption } from './components/Sidebar'
import { SettingsModal } from './components/SettingsModal'
import { StatsPanel, contextLabel, contextRatio } from './components/StatsPanel'
import { IconActivity, IconLayoutIde, IconPanelLeft, IconPanelRight, IconX } from './components/icons'
import { EditorPane, ExplorerPane, FileDrawer, type FilesContext } from './components/files/Workspace'
import { isMac } from './components/files/Explorer'
import { useMediaQuery } from './hooks/useMediaQuery'
import { useComposerVisibility } from './hooks/useComposerVisibility'
import { usePanelLayout } from './hooks/usePanelLayout'
import { useStatsPanel } from './hooks/useStatsPanel'
import { usePersistentSet, usePersistentState } from './hooks/usePersistentState'
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
  const [hiddenProjects, setHiddenProjects] = usePersistentSet('vva.projects-hidden')
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
  const [unreadIds, setUnreadIds] = usePersistentSet('vva.sessions-unread')
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
    explorerWidth,
    explorerResize,
    aiPanelWidth,
    aiPanelResize,
    mode,
    setMode,
    activePath,
    focus,
    focusSeq,
    tabs,
    drawerView,
    setDrawerView,
    searchSeq,
    openFile,
    closeTab,
    selectTab,
    resetFiles,
    requestSearch,
  } = usePanelLayout()
  const narrow = useMediaQuery('(max-width: 820px)')
  // IDE 模式仅桌面宽度生效；窄屏回落为对话模式布局
  const ide = mode === 'ide' && !narrow
  const [revealDir, setRevealDir] = useState<{ path: string; seq: number } | null>(null)

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
  const activeSessionId = active?.id
  const activeRoot = active?.workspace_root
  useEffect(() => {
    if (activeSessionId == null || activeRoot == null) return
    const name = projectName(activeRoot)
    setHiddenProjects(prev => {
      if (!prev.has(name)) return prev
      const next = new Set(prev)
      next.delete(name)
      return next
    })
  }, [activeSessionId, activeRoot, setHiddenProjects])

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
    setSnap(null)
    setPending(null)
    resetFiles()
    if (!activeId) return
    return subscribeSession(activeId, setSnap, e => setError(toError(e)))
  }, [activeId, resetFiles])

  const relatedFiles = useMemo(
    () => (active ? extractRelatedFiles(active, snap?.draft) : []),
    [active, snap?.draft],
  )

  const revealInExplorer = useCallback(
    (dir: string) => {
      setRevealDir({ path: dir, seq: Date.now() })
      setDrawerView('explorer')
    },
    [setDrawerView],
  )

  const filesCtx: FilesContext | null = active
    ? {
        session: active,
        draft: snap?.draft,
        files: relatedFiles,
        activePath,
        focus,
        focusSeq,
        searchSeq,
        revealDir,
        onOpen: openFile,
        onReveal: revealInExplorer,
      }
    : null

  // 全局快捷键：⌘/Ctrl+P 跳转文件，⌘/Ctrl+\ 切换对话 / IDE 模式
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (!(event.metaKey || event.ctrlKey) || event.altKey || !activeId) return
      if (event.key === 'p' || event.key === 'P') {
        event.preventDefault()
        requestSearch()
      } else if (event.key === '\\' && !narrow) {
        event.preventDefault()
        setMode(value => (value === 'ide' ? 'chat' : 'ide'))
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [activeId, narrow, requestSearch, setMode])

  // 会话在本地会话列表中保持同步（标题 / 模型 / 轮次 / 运行态）
  const summary = useMemo(
    () => active && snap?.persisted ? sessionSummary(active, snap.state) : null,
    [active, snap?.persisted, snap?.state],
  )
  // 服务端清单尚未收录的本地会话（已创建未持久化）：轮询合并时保留
  const localOnlyIds = useRef<Set<string>>(new Set())
  useEffect(() => {
    if (!summary) return
    localOnlyIds.current.delete(summary.id)
    setSessions(prev => [summary, ...prev.filter(item => item.id !== summary.id)])
  }, [summary])

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
              item.id === next.session.id ? sessionSummary(next.session, next.state) : item,
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

  // 定期刷新列表：有生成中会话时 4s 快速跟进（新派生的 subagent 会话由引擎侧
  // 创建，本地无通知）；全部空闲时 15s 兜底纠偏（如外部触发的运行态变化）
  const anyGenerating = sessions.some(session => Boolean(session.state))
  useEffect(() => {
    const timer = setInterval(() => {
      api
        .sessions()
        .then(list => {
          setSessions(prev => {
            for (const item of list) localOnlyIds.current.delete(item.id)
            // 保留本地未持久化条目；服务端已消失的（含级联删除的子会话）随之移除
            return [...list, ...prev.filter(item => localOnlyIds.current.has(item.id))]
          })
        })
        .catch(() => {})
    }, anyGenerating ? 4000 : 15000)
    return () => clearInterval(timer)
  }, [anyGenerating])

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

  const {
    stats,
    open: statsOpen,
    toggle: toggleStats,
    show: showStats,
  } = useStatsPanel({ activeId, turnCount })
  const {
    scrollProps,
    composerAreaRef,
    composerAreaProps,
    composerHidden,
    topbarHidden,
    revealComposer,
    followBottom,
  } = useComposerVisibility({ activeId, scrollRef })

  const lastVisibleTurn = useMemo(() => {
    const turns = snap?.session.turns ?? []
    for (let index = turns.length - 1; index >= 0; index--) {
      if (!turns[index].background_task) return turns[index]
    }
    return null
  }, [snap?.session.turns])

  // 正文与草稿增长时跟随；后台状态和隐藏的完成投递保留当前位置。
  useEffect(() => {
    followBottom()
  }, [lastVisibleTurn, snap?.draft, followBottom])

  // 请求失败统一走顶部 error banner
  const attempt = useCallback(async (action: () => Promise<unknown>) => {
    try {
      await action()
    } catch (e) {
      setError(toError(e))
    }
  }, [])

  const submit = useCallback(
    async (text: string, files: FileAttachment[] = []) => {
      setError(null)
      await attempt(async () => {
        let id = activeId
        if (!id) {
          const created = await api.createSession({
            model: draftModel ?? undefined,
            variant: draftVariant,
            search: draftSearch,
            workspace: draftProject || undefined,
          })
          id = created.session.id
          localOnlyIds.current.add(id)
          setActiveId(id)
        }
        await api.submit(id, text, files)
        setPending(text || null)
        setSessions(prev =>
          prev.map(item => (item.id === id ? { ...item, state: 'Thinking...' } : item)),
        )
      })
    },
    [activeId, draftModel, draftVariant, draftSearch, draftProject, attempt],
  )

  const abort = useCallback(async () => {
    if (activeId) await attempt(() => api.abort(activeId))
  }, [activeId, attempt])

  // 关闭结果通过会话快照同步给全部订阅者
  const dismissNotice = useCallback(
    (id: string) => {
      if (!activeId) return
      api.dismissNotice(activeId, id).catch(e => setError(toError(e)))
    },
    [activeId],
  )

  const answerUser = useCallback(
    async (answers: UserAnswer[]) => {
      if (!activeId || !snap?.pending_user_question) return
      const requestId = snap.pending_user_question.id
      await attempt(() => api.answerUser(activeId, requestId, answers))
    },
    [activeId, snap?.pending_user_question, attempt],
  )

  // 回退：丢弃该用户轮次及之后的内容，提示词放回输入框重新编辑
  const revertTurn = useCallback(
    async (turnIndex: number, prompt: string) => {
      if (!activeId) return
      revealComposer()
      await attempt(async () => {
        await api.revert(activeId, turnIndex)
        setPending(null)
        composerRef.current?.set(prompt)
      })
    },
    [activeId, revealComposer, attempt],
  )

  // 分叉：保留该用户轮次之前的历史创建新会话，并切换过去
  const forkTurn = useCallback(
    async (turnIndex: number) => {
      if (!activeId) return
      await attempt(async () => {
        const created = await api.fork(activeId, turnIndex)
        setActiveId(created.session.id)
      })
    },
    [activeId, attempt],
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
      await attempt(async () => {
        await api.deleteSession(id)
        localOnlyIds.current.delete(id)
        setSessions(prev => prev.filter(item => item.id !== id))
        if (activeId === id) setActiveId(null)
      })
    },
    [activeId, attempt],
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
      if (activeId) await attempt(() => api.configure(activeId, body))
    },
    [activeId, attempt],
  )

  const currentModel = active?.current_model ?? draftModel ?? models?.default ?? ''
  const currentVariant = active ? active.current_variant : draftVariant
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
      <label
        className="chip-toggle"
        title={
          snap && snap.external_web.length > 0
            ? `外置搜索：${snap.external_web.join('+')}`
            : '原生网络搜索'
        }
      >
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
      }${fileSidebarOpen && !ide ? ' file-sidebar-open' : ''}${ide && filesCtx ? ' ide' : ''}`}
      style={
        {
          '--sidebar-width': `${sidebarWidth}px`,
          '--file-sidebar-width': `${fileSidebarWidth}px`,
          '--ai-panel-width': `${aiPanelWidth}px`,
        } as CSSProperties
      }
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
      {ide && filesCtx && <ExplorerPane ctx={filesCtx} width={explorerWidth} />}
      {ide && filesCtx && (
        <Sash
          className="sash-explorer"
          label="拖拽调节文件列表宽度"
          onStart={explorerResize.onStart}
          onMove={explorerResize.onMove}
          onEnd={explorerResize.onEnd}
        />
      )}
      <main className={`main${topbarHidden ? ' topbar-hidden' : ''}`}>
        <header className="topbar">
          <button
            className="icon-ghost menu"
            onClick={() => {
              if (window.innerWidth <= 820 || ide) {
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
            <div className="stats-anchor">
              <button
                className={`icon-ghost stats-toggle${statsOpen ? ' active' : ''}`}
                onClick={toggleStats}
                title="用量统计"
                aria-label="用量统计"
                aria-expanded={statsOpen}
                data-stats-trigger
              >
                <IconActivity size={18} />
              </button>
              {statsOpen && <StatsPanel stats={stats} active={active} />}
            </div>
          )}
          {active && !narrow && (
            <button
              className={`icon-ghost mode-toggle${ide ? ' active' : ''}`}
              onClick={() => setMode(value => (value === 'ide' ? 'chat' : 'ide'))}
              title={`${ide ? '回到对话模式' : 'IDE 模式：文件 | 编辑区 | 对话'}（${isMac() ? '⌘' : 'Ctrl+'}\\）`}
              aria-label="IDE 模式"
              aria-pressed={ide}
            >
              <IconLayoutIde size={18} />
            </button>
          )}
          {active && !ide && (
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
        {error && (
          <div className="banner error" onClick={() => setError(null)}>
            {error}
          </div>
        )}
        {snap && snap.notices.length > 0 && (
          <div className="banner notice">
            {snap.notices.map(notice => (
              <div key={notice.id} className="notice-row">
                {notice.kind !== 'capability' && (
                  <button
                    className="notice-dismiss"
                    aria-label="关闭提示"
                    title="关闭提示"
                    onClick={() => dismissNotice(notice.id)}
                  >
                    <IconX size={12} />
                  </button>
                )}
                <span className="notice-text">{notice.text}</span>
              </div>
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
            onError={setError}
          />
        ) : (
          <>
            <div className="chat-scroll" ref={scrollRef} {...scrollProps}>
              <Chat
                snap={snap}
                pending={pending}
                onRevert={revertTurn}
                onFork={forkTurn}
                onPreviewFile={openFile}
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
                onError={setError}
                controls={composerControls}
                skills={skills}
                workspace={active?.workspace_root}
                contextRatio={stats ? contextRatio(stats) : undefined}
                contextText={stats ? contextLabel(stats) : undefined}
                onOpenStats={showStats}
              />
            </div>
          </>
        )}
      </main>
      {filesCtx && !ide && (
        <>
          <Sash
            className="sash-files"
            label="拖拽调节文件面板宽度"
            onStart={fileSidebarResize.onStart}
            onMove={fileSidebarResize.onMove}
            onEnd={fileSidebarResize.onEnd}
          />
          <FileDrawer
            ctx={filesCtx}
            open={fileSidebarOpen}
            width={fileSidebarWidth}
            view={drawerView}
            onBack={() => setDrawerView('explorer')}
            onClose={() => setFileSidebarOpen(false)}
            onSwitchIde={narrow ? undefined : () => setMode('ide')}
          />
          <div className="file-backdrop" onClick={() => setFileSidebarOpen(false)} />
        </>
      )}
      {filesCtx && ide && (
        <>
          <EditorPane ctx={filesCtx} tabs={tabs} onSelectTab={selectTab} onCloseTab={closeTab} />
          <Sash
            className="sash-ai"
            label="拖拽调节对话栏宽度"
            onStart={aiPanelResize.onStart}
            onMove={aiPanelResize.onMove}
            onEnd={aiPanelResize.onEnd}
          />
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
