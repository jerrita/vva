import type { PointerEvent as ReactPointerEvent } from 'react'

/** 可拖拽分隔柱：拖拽期间加 is-resizing 样式，Pointer Capture 跟随指针。 */
export function Sash({
  className,
  label,
  onStart,
  onMove,
  onEnd,
}: {
  className?: string
  label: string
  onStart: (event: ReactPointerEvent<HTMLDivElement>) => void
  onMove: (event: ReactPointerEvent<HTMLDivElement>) => void
  onEnd: (event: ReactPointerEvent<HTMLDivElement>) => void
}) {
  return (
    <div
      className={`sash${className ? ` ${className}` : ''}`}
      onPointerDown={event => {
        event.currentTarget.classList.add('is-resizing')
        onStart(event)
      }}
      onPointerMove={onMove}
      onPointerUp={event => {
        event.currentTarget.classList.remove('is-resizing')
        onEnd(event)
      }}
      onPointerCancel={event => {
        event.currentTarget.classList.remove('is-resizing')
        onEnd(event)
      }}
      title={label}
      role="separator"
      aria-orientation="vertical"
      aria-label={label}
    >
      <span className="sash-grip" aria-hidden>
        <span />
        <span />
        <span />
      </span>
    </div>
  )
}
