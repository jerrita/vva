import { useCallback, useState } from 'react'
import { useResizable } from './useResizable'
import { usePersistentFlag, usePersistentState } from './usePersistentState'

const SIDEBAR_MIN = 200
const SIDEBAR_MAX = 420
const FILE_SIDEBAR_MIN = 240
const FILE_SIDEBAR_MAX = 1000

const clampSidebar = (width: number) =>
  Math.min(SIDEBAR_MAX, Math.max(SIDEBAR_MIN, width))
const clampFileSidebar = (width: number) =>
  Math.min(FILE_SIDEBAR_MAX, Math.max(FILE_SIDEBAR_MIN, width))

/**
 * 工作台面板布局：会话侧栏与文件抽屉的持久化开合/宽度与拖拽调节，
 * 以及侧栏文件预览目标路径（预览时自动展开并加宽文件抽屉）。
 */
export function usePanelLayout() {
  const [sidebarHidden, setSidebarHidden] = usePersistentFlag('vva.sidebar-hidden')
  const [sidebarWidth, setSidebarWidth] = usePersistentState(
    'vva.sidebar-width',
    () => {
      const stored = Number(localStorage.getItem('vva.sidebar-width'))
      return stored >= SIDEBAR_MIN && stored <= SIDEBAR_MAX ? stored : 268
    },
    String,
  )
  const [fileSidebarOpen, setFileSidebarOpen] = usePersistentFlag('vva.file-sidebar-open')
  const [fileSidebarWidth, setFileSidebarWidth] = usePersistentState(
    'vva.file-sidebar-width',
    () => {
      const stored = Number(localStorage.getItem('vva.file-sidebar-width'))
      return stored >= FILE_SIDEBAR_MIN && stored <= FILE_SIDEBAR_MAX ? stored : 360
    },
    String,
  )
  const [previewFilePath, setPreviewFilePath] = useState<string | null>(null)

  const sidebarResize = useResizable(() => sidebarWidth, setSidebarWidth, clampSidebar, 1)
  const fileSidebarResize = useResizable(
    () => fileSidebarWidth,
    setFileSidebarWidth,
    clampFileSidebar,
    -1,
  )

  // 聊天流点击文件路径：展开抽屉并保证足够展示宽度
  const onPreviewFile = useCallback(
    (path: string) => {
      setPreviewFilePath(path)
      setFileSidebarOpen(true)
      setFileSidebarWidth(w => clampFileSidebar(Math.max(w, 680)))
    },
    [setFileSidebarOpen, setFileSidebarWidth],
  )

  return {
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
  }
}
