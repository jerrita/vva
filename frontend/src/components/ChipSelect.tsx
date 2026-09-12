import { useEffect, useRef, useState } from 'react'
import type { KeyboardEvent as ReactKeyboardEvent } from 'react'

export interface ChipOption {
  value: string
  label: string
}

/** 输入区 chip 下拉：以主题化浮层替代原生 select 的浏览器弹出列表 */
export function ChipSelect({
  value,
  options,
  onChange,
  title,
}: {
  value: string
  options: ChipOption[]
  onChange: (value: string) => void
  title: string
}) {
  const [open, setOpen] = useState(false)
  const [active, setActive] = useState(0)
  const rootRef = useRef<HTMLDivElement>(null)
  const activeRef = useRef<HTMLButtonElement>(null)

  const current = options.find(option => option.value === value)

  const openMenu = () => {
    setActive(Math.max(0, options.findIndex(option => option.value === value)))
    setOpen(true)
  }

  const select = (option: ChipOption | undefined) => {
    if (!option) return
    onChange(option.value)
    setOpen(false)
  }

  // 浮层为瞬态菜单，点击外部 / Esc 关闭
  useEffect(() => {
    if (!open) return
    const onPointerDown = (event: PointerEvent) => {
      if (rootRef.current?.contains(event.target as Node)) return
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

  // 键盘导航与选中，对齐原生 select 的操作习惯
  const onKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    if (!open) {
      if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
        event.preventDefault()
        openMenu()
      }
      return
    }
    if (event.key === 'ArrowDown') {
      event.preventDefault()
      setActive(index => (index + 1) % options.length)
    } else if (event.key === 'ArrowUp') {
      event.preventDefault()
      setActive(index => (index - 1 + options.length) % options.length)
    } else if (event.key === 'Enter') {
      event.preventDefault()
      select(options[active])
    }
  }

  useEffect(() => {
    if (open) activeRef.current?.scrollIntoView({ block: 'nearest' })
  }, [open, active])

  return (
    <div className={`chip-select${open ? ' open' : ''}`} ref={rootRef} onKeyDown={onKeyDown}>
      <button
        className="chip-trigger"
        aria-haspopup="listbox"
        aria-expanded={open}
        title={title}
        onClick={() => (open ? setOpen(false) : openMenu())}
      >
        <span className="chip-label">{current?.label ?? value}</span>
        <svg
          viewBox="0 0 24 24"
          width="10"
          height="10"
          fill="none"
          stroke="currentColor"
          strokeWidth="2.5"
          strokeLinecap="round"
          strokeLinejoin="round"
          aria-hidden
        >
          <path d="M6 9l6 6 6-6" />
        </svg>
      </button>
      {open && (
        <div className="chip-menu" role="listbox" aria-label={title}>
          {options.map((option, index) => (
            <button
              key={option.value}
              ref={index === active ? activeRef : undefined}
              role="option"
              aria-selected={option.value === value}
              className={`chip-option${option.value === value ? ' selected' : ''}`}
              onMouseEnter={() => setActive(index)}
              onClick={() => select(option)}
              title={option.label}
            >
              <span className="chip-option-label">{option.label}</span>
              {option.value === value && (
                <svg
                  viewBox="0 0 24 24"
                  width="13"
                  height="13"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="2.5"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  aria-hidden
                >
                  <path d="M20 6L9 17l-5-5" />
                </svg>
              )}
            </button>
          ))}
        </div>
      )}
    </div>
  )
}
