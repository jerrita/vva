import { useCallback, useState } from 'react'
import { useResizable } from './useResizable'
import { usePersistentFlag, usePersistentState } from './usePersistentState'

const SIDEBAR_MIN = 200
const SIDEBAR_MAX = 420
const FILE_SIDEBAR_MIN = 300
const FILE_SIDEBAR_MAX = 1000
/** 抽屉打开文件时的最小阅读宽度 */
const FILE_VIEW_MIN = 560
const EXPLORER_MIN = 200
const EXPLORER_MAX = 440
const AI_PANEL_MIN = 360
const AI_PANEL_MAX = 720
/** IDE 模式最多保留的标签数：超出时丢弃最早且非当前的标签 */
const MAX_TABS = 12

const clamp = (min: number, max: number) => (width: number) => Math.min(max, Math.max(min, width))
const clampSidebar = clamp(SIDEBAR_MIN, SIDEBAR_MAX)
const clampFileSidebar = clamp(FILE_SIDEBAR_MIN, FILE_SIDEBAR_MAX)
const clampExplorer = clamp(EXPLORER_MIN, EXPLORER_MAX)
const clampAiPanel = clamp(AI_PANEL_MIN, AI_PANEL_MAX)

function persistedWidth(key: string, min: number, max: number, fallback: number) {
  return usePersistentState(
    key,
    () => {
      const stored = Number(localStorage.getItem(key))
      return stored >= min && stored <= max ? stored : fallback
    },
    String,
  )
}

export type LayoutMode = 'chat' | 'ide'

/** 打开文件时的定位目标：读取区间（高亮并滚入视区）或本会话改动处 */
export type FileFocus = { start: number; end: number } | 'changes' | null

/** 聊天流/浏览器打开文件的回调 */
export type PreviewFile = (path: string, focus?: FileFocus) => void

/**
 * 工作台布局：会话侧栏、文件抽屉（对话模式）与 IDE 三栏（文件 | 编辑区 | AI）的
 * 持久化开合/宽度与拖拽调节，以及打开的文件、标签与定位目标。
 */
export function usePanelLayout() {
  const [sidebarHidden, setSidebarHidden] = usePersistentFlag('vva.sidebar-hidden')
  const [sidebarWidth, setSidebarWidth] = persistedWidth('vva.sidebar-width', SIDEBAR_MIN, SIDEBAR_MAX, 268)
  const [fileSidebarOpen, setFileSidebarOpen] = usePersistentFlag('vva.file-sidebar-open')
  const [fileSidebarWidth, setFileSidebarWidth] = persistedWidth(
    'vva.file-sidebar-width',
    FILE_SIDEBAR_MIN,
    FILE_SIDEBAR_MAX,
    420,
  )
  const [explorerWidth, setExplorerWidth] = persistedWidth('vva.explorer-width', EXPLORER_MIN, EXPLORER_MAX, 260)
  const [aiPanelWidth, setAiPanelWidth] = persistedWidth('vva.ai-panel-width', AI_PANEL_MIN, AI_PANEL_MAX, 460)
  const [mode, setMode] = usePersistentState<LayoutMode>(
    'vva.layout-mode',
    () => (localStorage.getItem('vva.layout-mode') === 'ide' ? 'ide' : 'chat'),
    value => value,
  )

  /** 当前文件（两种模式共用）；抽屉内是否显示文件由 drawerView 决定 */
  const [activePath, setActivePath] = useState<string | null>(null)
  const [focus, setFocus] = useState<FileFocus>(null)
  /** 定位请求序号：同一文件重复点击也重新定位 */
  const [focusSeq, setFocusSeq] = useState(0)
  const [tabs, setTabs] = useState<string[]>([])
  const [drawerView, setDrawerView] = useState<'explorer' | 'file'>('explorer')
  /** 请求聚焦文件搜索框（⌘P）的序号 */
  const [searchSeq, setSearchSeq] = useState(0)

  const sidebarResize = useResizable(() => sidebarWidth, setSidebarWidth, clampSidebar, 1)
  const fileSidebarResize = useResizable(() => fileSidebarWidth, setFileSidebarWidth, clampFileSidebar, -1)
  const explorerResize = useResizable(() => explorerWidth, setExplorerWidth, clampExplorer, 1)
  const aiPanelResize = useResizable(() => aiPanelWidth, setAiPanelWidth, clampAiPanel, -1)

  const openFile = useCallback(
    (path: string, target: FileFocus = null) => {
      setActivePath(path)
      setFocus(target)
      setFocusSeq(seq => seq + 1)
      setTabs(prev => {
        if (prev.includes(path)) return prev
        const next = [...prev, path]
        return next.length > MAX_TABS ? next.slice(next.length - MAX_TABS) : next
      })
      setDrawerView('file')
      setFileSidebarOpen(true)
      setFileSidebarWidth(width => clampFileSidebar(Math.max(width, FILE_VIEW_MIN)))
    },
    [setFileSidebarOpen, setFileSidebarWidth],
  )

  const closeTab = useCallback((path: string) => {
    setTabs(prev => {
      const index = prev.indexOf(path)
      if (index === -1) return prev
      const next = prev.filter(item => item !== path)
      setActivePath(current => (current === path ? (next[index] ?? next[index - 1] ?? null) : current))
      return next
    })
  }, [])

  /** 切换标签：不继承上一文件的定位目标 */
  const selectTab = useCallback((path: string) => {
    setActivePath(path)
    setFocus(null)
  }, [])

  /** 切换会话：清空文件上下文 */
  const resetFiles = useCallback(() => {
    setActivePath(null)
    setFocus(null)
    setTabs([])
    setDrawerView('explorer')
  }, [])

  /** ⌘P：展示文件搜索（对话模式打开抽屉并回到浏览视图） */
  const requestSearch = useCallback(() => {
    setFileSidebarOpen(true)
    setDrawerView('explorer')
    setSearchSeq(seq => seq + 1)
  }, [setFileSidebarOpen])

  return {
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
  }
}
