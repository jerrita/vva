import { Fragment, useCallback, useEffect, useMemo, useRef, useState } from 'react'
import type { FocusEvent as ReactFocusEvent, PointerEvent as ReactPointerEvent, TouchEvent as ReactTouchEvent } from 'react'
import { api, toError } from './api'
import { subscribeSession } from './sessionSync'
import type { ConfigureBody } from './api'
import { Chat, turnText } from './components/Chat'
import { AskUserModal } from './components/AskUserModal'
import { ChipSelect } from './components/ChipSelect'
import { Composer, type ComposerHandle } from './components/Composer'
import { Sidebar, groupByProject, projectName } from './components/Sidebar'
import { SettingsModal } from './components/SettingsModal'
import { IconActivity, IconPanelLeft, IconPanelRight } from './components/icons'
import {
  FileSidebar,
  FILE_SIDEBAR_MIN,
  FILE_SIDEBAR_MAX,
  clampFileSidebar,
  extractRelatedFiles,
} from './components/FileSidebar'
import type { ProjectOption } from './components/Sidebar'
import type { FileAttachment, ModelsResponse, SessionSnapshot, SessionSummary, SidebarStats, SkillEntry, UserAnswer } from './types'

const SIDEBAR_MIN = 200
const SIDEBAR_MAX = 420

const clampSidebar = (width: number) =>
  Math.min(SIDEBAR_MAX, Math.max(SIDEBAR_MIN, width))

const MOBILE_MQ = '(max-width: 820px)'
const COMPOSER_HIDE_DELTA = 16
const COMPOSER_EDGE = 72
const COMPOSER_SWIPE = 24

function viewportBottom(): number {
  const viewport = window.visualViewport
  return viewport ? viewport.offsetTop + viewport.height : window.innerHeight
}

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
  const [skills, setSkills] = useState<SkillEntry[]>([])
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
  // AskUser 提问挂起：面板停靠输入框上方，输入区保持展示可交互
  const askPending = snap?.pending_user_question != null
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
  const [composerHidden, setComposerHidden] = useState(false)
  const [topbarHidden, setTopbarHidden] = useState(false)

  const composerHiddenRef = useRef(false)
  composerHiddenRef.current = composerHidden
  const composerAreaRef = useRef<HTMLDivElement>(null)
  const composerFocused = useRef(false)
  const mobileRef = useRef(false)
  const lastScrollTop = useRef(0)
  const touchStartY = useRef<number | null>(null)
  const composerTouchStartY = useRef<number | null>(null)
  // 下滑隐藏后的抑制窗口：隐藏引发的 clamp 滚动会被判定为"在底部"而立即回显
  const suppressRevealUntil = useRef(0)

  useEffect(() => {
    api.models().then(setModels).catch(e => setError(toError(e)))
    api.workspace().then(workspace => setDefaultRoot(workspace.root)).catch(() => {})
    api.sessions().then(setSessions).catch(e => setError(toError(e)))
    api.skills().then(response => setSkills(response.skills)).catch(() => {})
  }, [])

  useEffect(() => {
    const mq = window.matchMedia(MOBILE_MQ)
    const sync = () => {
      mobileRef.current = mq.matches
      if (!mq.matches) setComposerHidden(false)
    }
    sync()
    mq.addEventListener('change', sync)
    return () => mq.removeEventListener('change', sync)
  }, [])

  // 切换会话时输入框复位为展示
  useEffect(() => {
    setComposerHidden(false)
    setTopbarHidden(false)
    nearBottom.current = true
    lastScrollTop.current = 0
  }, [activeId])

  // 窄屏隐藏用负 margin 回收高度，跟踪输入区实测高度
  useEffect(() => {
    const area = composerAreaRef.current
    if (!area) return
    const apply = () => {
      area.style.setProperty('--composer-h', `${area.offsetHeight}px`)
    }
    apply()
    const observer = new ResizeObserver(apply)
    observer.observe(area)
    return () => observer.disconnect()
  }, [activeId])

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
    if (el && nearBottom.current) {
      el.scrollTop = el.scrollHeight
      if (Date.now() >= suppressRevealUntil.current) setComposerHidden(false)
    }
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
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 80
    nearBottom.current = atBottom
    refreshStatsOverlap()
    if (!mobileRef.current) {
      lastScrollTop.current = el.scrollTop
      return
    }
    if (atBottom) {
      lastScrollTop.current = el.scrollTop
      if (Date.now() >= suppressRevealUntil.current) setComposerHidden(false)
      setTopbarHidden(false)
      return
    }
    const prev = lastScrollTop.current
    lastScrollTop.current = el.scrollTop
    const delta = el.scrollTop - prev
    // 下滑回看历史：隐藏标题栏；上滑恢复
    if (delta < -COMPOSER_HIDE_DELTA) {
      setTopbarHidden(true)
    } else if (delta > COMPOSER_HIDE_DELTA) {
      setTopbarHidden(false)
    }
    // 输入中不隐藏输入框
    if (composerFocused.current) return
    if (delta < -COMPOSER_HIDE_DELTA) setComposerHidden(true)
  }

  const onChatTouchStart = (event: ReactTouchEvent<HTMLDivElement>) => {
    if (!mobileRef.current || !composerHiddenRef.current) {
      touchStartY.current = null
      return
    }
    const y = event.touches[0]?.clientY
    if (y == null) return
    touchStartY.current = viewportBottom() - y <= COMPOSER_EDGE ? y : null
  }

  const onChatTouchMove = (event: ReactTouchEvent<HTMLDivElement>) => {
    const start = touchStartY.current
    const y = event.touches[0]?.clientY
    if (start == null || y == null) return
    if (start - y >= COMPOSER_SWIPE) {
      setComposerHidden(false)
      touchStartY.current = null
    }
  }

  const onComposerFocus = () => {
    composerFocused.current = true
    setComposerHidden(false)
  }

  const onComposerBlur = (event: ReactFocusEvent<HTMLDivElement>) => {
    const next = event.relatedTarget
    if (!(next instanceof Node) || !event.currentTarget.contains(next)) {
      composerFocused.current = false
    }
  }

  // 移动端：输入框上下滑隐藏
  const onComposerTouchStart = (event: ReactTouchEvent<HTMLDivElement>) => {
    composerTouchStartY.current = event.touches[0]?.clientY ?? null
  }

  const onComposerTouchMove = (event: ReactTouchEvent<HTMLDivElement>) => {
    if (!mobileRef.current) return
    const start = composerTouchStartY.current
    const y = event.touches[0]?.clientY
    if (start == null || y == null) return
    if (y - start < COMPOSER_SWIPE) return
    composerTouchStartY.current = null
    // 文本框内容滚动中不触发
    const target = event.target
    if (target instanceof HTMLTextAreaElement && target.scrollTop > 0) return
    suppressRevealUntil.current = Date.now() + 400
    setComposerHidden(true)
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
      setComposerHidden(false)
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
    >
      <Sidebar
        groups={groupByProject(sessions)}
        projects={projects}
        activeId={activeId}
        width={sidebarWidth}
        skills={skills}
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
          <div
            className={`stats-panel${statsVisible ? '' : ' hidden'}`}
            ref={statsPanelRef}
          >
            {(
              [
                ['Session', active.id],
                ['Workspace', workspaceName],
                ['Editor', currentEditor],
                ['Turns', fmtNum(stats.total_turns)],
                ['Context', `${fmtNum(stats.context_tokens)} / ${fmtLimit(contextLimit)}`],
                ['In', fmtNum(stats.prompt_tokens)],
                ['Out', fmtNum(stats.completion_tokens)],
                ...(stats.tps != null ? ([['TPS', stats.tps.toFixed(1)]] as const) : []),
                ['Cache Read', fmtNum(stats.cache_read_tokens)],
                ['Cache Write', fmtNum(stats.cache_write_tokens)],
                ['Est. Cost', `$${stats.estimated_cost_usd.toFixed(4)}`],
              ] as [string, string][]
            ).map(([label, value]) => (
              <Fragment key={label}>
                <div className="stat-row">
                  <span className="stat-label">{label}</span>
                  <span className="stat-value">{value}</span>
                </div>
                {label === 'Context' && (
                  <div className="ctx-gauge">
                    <span className="ctx-gauge-track">
                      <span className="ctx-gauge-fill" style={{ width: `${contextPercent}%` }} />
                    </span>
                    <span>{contextPercent}%</span>
                  </div>
                )}
              </Fragment>
            ))}
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
              <Composer running={running} onSend={submit} onStop={abort} controls={composerControls} skills={skills} />
            </div>
          </div>
        ) : (
          <>
            <div
              className="chat-scroll"
              ref={scrollRef}
              onScroll={onScroll}
              onTouchStart={onChatTouchStart}
              onTouchMove={onChatTouchMove}
            >
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
              onFocus={onComposerFocus}
              onBlur={onComposerBlur}
              onTouchStart={onComposerTouchStart}
              onTouchMove={onComposerTouchMove}
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
              />
            </div>
          </>
        )}
      </main>
      {active && (
        <>
          <FileSidebar
            sessionId={activeId}
            workspace={active.workspace_root}
            files={relatedFiles}
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
