import { useRef } from 'react'
import type { PointerEvent as ReactPointerEvent } from 'react'

/**
 * 面板宽度拖拽（Pointer Capture）：direction 为 +1 时指针右移加宽（左侧面板），
 * -1 时指针左移加宽（右侧面板），clamp 收敛宽度边界。
 */
export function useResizable(
  getWidth: () => number,
  setWidth: (width: number) => void,
  clamp: (width: number) => number,
  direction: 1 | -1,
) {
  const active = useRef(false)
  const startX = useRef(0)
  const startW = useRef(0)

  const onStart = (event: ReactPointerEvent<HTMLDivElement>) => {
    event.preventDefault()
    active.current = true
    startX.current = event.clientX
    startW.current = getWidth()
    document.body.classList.add('resizing')
    event.currentTarget.setPointerCapture(event.pointerId)
  }

  const onMove = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (!active.current) return
    setWidth(clamp(startW.current + direction * (event.clientX - startX.current)))
  }

  const onEnd = (event: ReactPointerEvent<HTMLDivElement>) => {
    if (!active.current) return
    active.current = false
    document.body.classList.remove('resizing')
    event.currentTarget.releasePointerCapture(event.pointerId)
  }

  return { onStart, onMove, onEnd }
}
