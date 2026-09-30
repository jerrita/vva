import { useCallback, useEffect, useState } from 'react'
import { api } from '../api'
import type { SidebarStats } from '../types'

/**
 * 用量统计状态：按会话与轮次拉取统计；弹层默认关闭，
 * 点击外部（带 `data-stats-popover` / `data-stats-trigger` 的元素除外）、Esc、切换会话时关闭。
 */
export function useStatsPanel(options: { activeId: string | null; turnCount: number }) {
  const { activeId, turnCount } = options
  const [stats, setStats] = useState<SidebarStats | null>(null)
  const [open, setOpen] = useState(false)

  // 切换会话时清空旧统计并关闭弹层
  useEffect(() => {
    setStats(null)
    setOpen(false)
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

  useEffect(() => {
    if (!open) return
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as Element | null
      if (target?.closest('[data-stats-popover], [data-stats-trigger]')) return
      setOpen(false)
    }
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setOpen(false)
    }
    document.addEventListener('pointerdown', onPointerDown)
    document.addEventListener('keydown', onKeyDown)
    return () => {
      document.removeEventListener('pointerdown', onPointerDown)
      document.removeEventListener('keydown', onKeyDown)
    }
  }, [open])

  const toggle = useCallback(() => setOpen(value => !value), [])
  const show = useCallback(() => setOpen(true), [])

  return { stats, open: open && stats !== null, toggle, show }
}
