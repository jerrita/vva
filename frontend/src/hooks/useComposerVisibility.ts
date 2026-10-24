import { useCallback, useEffect, useRef, useState } from 'react'
import type { FocusEvent as ReactFocusEvent, RefObject, TouchEvent as ReactTouchEvent, WheelEvent as ReactWheelEvent } from 'react'

const MOBILE_MQ = '(max-width: 820px)'
const COMPOSER_HIDE_DELTA = 16
const COMPOSER_EDGE = 72
const COMPOSER_SWIPE = 24

function viewportBottom(): number {
  const viewport = window.visualViewport
  return viewport ? viewport.offsetTop + viewport.height : window.innerHeight
}

/**
 * 移动端输入框 / 顶栏显隐手势机：滚动上滑隐藏输入框、上滑隐藏顶栏、
 * 屏幕底缘上滑唤出输入框、输入框内下滑隐藏（聚焦时除外）。
 * 桌面端仅维护底部跟随标记。
 */
export function useComposerVisibility(options: {
  activeId: string | null
  /** 聊天滚动容器 */
  scrollRef: RefObject<HTMLDivElement | null>
}) {
  const { activeId, scrollRef } = options

  const [composerHidden, setComposerHidden] = useState(false)
  const [topbarHidden, setTopbarHidden] = useState(false)

  const nearBottom = useRef(true)
  const composerHiddenRef = useRef(false)
  composerHiddenRef.current = composerHidden
  const composerAreaRef = useRef<HTMLDivElement>(null)
  const composerFocused = useRef(false)
  const mobileRef = useRef(false)
  const lastScrollTop = useRef(0)
  const lastScrollHeight = useRef(0)
  const touchLastY = useRef<number | null>(null)
  const touchStartY = useRef<number | null>(null)
  const composerTouchStartY = useRef<number | null>(null)
  // 下滑隐藏后的抑制窗口：隐藏引发的 clamp 滚动会被判定为"在底部"而立即回显
  const suppressRevealUntil = useRef(0)

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

  // 切换会话时输入框复位为展示，跟随标记同步归零
  useEffect(() => {
    setComposerHidden(false)
    setTopbarHidden(false)
    nearBottom.current = true
    lastScrollTop.current = 0
    lastScrollHeight.current = 0
    touchLastY.current = null
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

  const revealComposer = useCallback(() => {
    if (Date.now() >= suppressRevealUntil.current) setComposerHidden(false)
  }, [])

  // 用户位于底部时跟随滚动
  const followBottom = useCallback(() => {
    const el = scrollRef.current
    if (el && nearBottom.current) {
      el.scrollTop = el.scrollHeight
      revealComposer()
    }
  }, [revealComposer, scrollRef])

  const onScroll = useCallback(() => {
    const el = scrollRef.current
    if (!el) return
    const delta = el.scrollTop - lastScrollTop.current
    const heightDelta = el.scrollHeight - lastScrollHeight.current
    lastScrollTop.current = el.scrollTop
    lastScrollHeight.current = el.scrollHeight
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 80
    // 跟随判定按滚动方向：向上滚动一律脱离（底部阈值内的少量上滑也算，
    // 否则流式增长下永远逃不出阈值）；内容收缩引发的贴底钳制不算用户上滑
    if (delta < 0 && !(heightDelta < 0 && atBottom)) {
      nearBottom.current = false
    } else if (atBottom) {
      nearBottom.current = true
    }
    if (!mobileRef.current) return
    if (atBottom) {
      revealComposer()
      setTopbarHidden(false)
      return
    }
    // 上滑隐藏标题栏；下滑恢复
    if (delta > COMPOSER_HIDE_DELTA) {
      setTopbarHidden(true)
    } else if (delta < -COMPOSER_HIDE_DELTA) {
      setTopbarHidden(false)
    }
    // 输入中不隐藏输入框
    if (composerFocused.current) return
    if (delta < -COMPOSER_HIDE_DELTA) setComposerHidden(true)
  }, [revealComposer, scrollRef])

  // 上滑意图即刻脱离跟随：scroll 事件到帧末才触发，流式跟随写入若同帧先行，
  // 会覆盖用户刚滚上的位置并被判定为仍在底部，形成强制拉回；
  // 内容未溢出（scrollTop 为 0）时不算脱离，保证首次溢出仍跟随
  const unstickFollow = useCallback(() => {
    const el = scrollRef.current
    if (el && el.scrollTop > 0) nearBottom.current = false
  }, [scrollRef])

  const onWheel = useCallback(
    (event: ReactWheelEvent<HTMLDivElement>) => {
      if (event.deltaY < 0) unstickFollow()
    },
    [unstickFollow],
  )

  const onChatTouchStart = useCallback((event: ReactTouchEvent<HTMLDivElement>) => {
    touchStartY.current = null
    touchLastY.current = null
    if (!mobileRef.current) return
    const y = event.touches[0]?.clientY
    if (y == null) return
    touchLastY.current = y
    if (composerHiddenRef.current && viewportBottom() - y <= COMPOSER_EDGE) {
      touchStartY.current = y
    }
  }, [])

  const onChatTouchMove = useCallback((event: ReactTouchEvent<HTMLDivElement>) => {
    const y = event.touches[0]?.clientY
    if (mobileRef.current && y != null) {
      // 手指下移 = 内容上滚，即刻脱离跟随（理由同滚轮）
      if (touchLastY.current != null && y > touchLastY.current) unstickFollow()
      touchLastY.current = y
    }
    const start = touchStartY.current
    if (start == null || y == null) return
    if (start - y >= COMPOSER_SWIPE) {
      setComposerHidden(false)
      touchStartY.current = null
    }
  }, [unstickFollow])

  const onComposerFocus = useCallback(() => {
    composerFocused.current = true
    setComposerHidden(false)
  }, [])

  const onComposerBlur = useCallback((event: ReactFocusEvent<HTMLDivElement>) => {
    const next = event.relatedTarget
    if (!(next instanceof Node) || !event.currentTarget.contains(next)) {
      composerFocused.current = false
    }
  }, [])

  // 移动端：输入框上下滑隐藏
  const onComposerTouchStart = useCallback((event: ReactTouchEvent<HTMLDivElement>) => {
    composerTouchStartY.current = event.touches[0]?.clientY ?? null
  }, [])

  const onComposerTouchMove = useCallback((event: ReactTouchEvent<HTMLDivElement>) => {
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
  }, [])

  return {
    scrollRef,
    scrollProps: {
      onScroll,
      onWheel,
      onTouchStart: onChatTouchStart,
      onTouchMove: onChatTouchMove,
    },
    composerAreaRef,
    composerAreaProps: {
      onFocus: onComposerFocus,
      onBlur: onComposerBlur,
      onTouchStart: onComposerTouchStart,
      onTouchMove: onComposerTouchMove,
    },
    composerHidden,
    topbarHidden,
    revealComposer,
    followBottom,
  }
}
