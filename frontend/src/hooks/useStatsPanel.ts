import { useCallback, useEffect, useRef, useState } from 'react'
import { api } from '../api'
import { usePersistentFlag } from './usePersistentState'
import type { RefObject } from 'react'
import type { SidebarStats } from '../types'

/**
 * 用量面板状态：按会话与轮次拉取统计；遮挡自动收起（内容滚入面板区域时隐藏，
 * 离开后恢复）；顶栏开关为手动覆盖——隐藏时点击强制显示（保持到不再遮挡）。
 */
export function useStatsPanel(options: {
  activeId: string | null
  turnCount: number
  /** 内容变化信号（快照对象身份），流式增量时触发遮挡重估 */
  contentKey: unknown
  scrollRef: RefObject<HTMLDivElement | null>
}) {
  const { activeId, turnCount, contentKey, scrollRef } = options
  const [stats, setStats] = useState<SidebarStats | null>(null)
  const [statsCollapsed, setStatsCollapsed] = usePersistentFlag('vva.stats-collapsed')
  const [statsAutoHidden, setStatsAutoHidden] = useState(false)
  const [statsPinned, setStatsPinned] = useState(false)
  const statsPanelRef = useRef<HTMLDivElement>(null)
  const statsOverlapRaf = useRef(0)

  // 切换会话时清空旧统计，避免展示上一会话的数据
  useEffect(() => {
    setStats(null)
  }, [activeId])

  // 切换会话与轮次变化时刷新
  useEffect(() => {
    if (!activeId) return
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

  const statsVisible = stats !== null && !statsCollapsed && (!statsAutoHidden || statsPinned)

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
  }, [scrollRef])

  // 卸载时取消待执行的遮挡检测帧
  useEffect(
    () => () => {
      if (statsOverlapRaf.current) cancelAnimationFrame(statsOverlapRaf.current)
    },
    [],
  )

  // 内容变化（流式增量、切换会话、手动开合）后重新评估遮挡
  useEffect(() => {
    refreshStatsOverlap()
  }, [contentKey, stats, statsCollapsed, statsPinned, refreshStatsOverlap])

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
  }, [activeId, refreshStatsOverlap, scrollRef])

  const toggle = useCallback(() => {
    if (statsVisible) {
      setStatsCollapsed(true)
      setStatsPinned(false)
    } else {
      setStatsCollapsed(false)
      // 遮挡位置强制显示，保持到滚动至不再遮挡
      setStatsPinned(true)
    }
  }, [statsVisible, setStatsCollapsed])

  return { stats, statsVisible, statsPanelRef, toggle, refreshStatsOverlap }
}
