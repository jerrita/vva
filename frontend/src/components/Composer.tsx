import { useEffect, useRef, useState } from 'react'
import type { KeyboardEvent } from 'react'

export function Composer({
  running,
  onSend,
  onStop,
}: {
  running: boolean
  onSend: (text: string) => void
  onStop: () => void
}) {
  const [text, setText] = useState('')
  const textareaRef = useRef<HTMLTextAreaElement>(null)

  const resize = () => {
    const el = textareaRef.current
    if (!el) return
    el.style.height = 'auto'
    el.style.height = `${Math.min(el.scrollHeight, 200)}px`
  }

  const send = () => {
    const trimmed = text.trim()
    if (!trimmed || running) return
    onSend(trimmed)
    setText('')
    requestAnimationFrame(resize)
  }

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === 'Enter' && !event.shiftKey && !event.nativeEvent.isComposing) {
      event.preventDefault()
      send()
    }
  }

  useEffect(() => {
    if (!running) textareaRef.current?.focus()
  }, [running])

  return (
    <div className="composer">
      <textarea
        ref={textareaRef}
        value={text}
        rows={1}
        placeholder={running ? '正在执行…' : '输入消息，Enter 发送，Shift+Enter 换行'}
        onChange={event => {
          setText(event.target.value)
          resize()
        }}
        onKeyDown={onKeyDown}
      />
      {running ? (
        <button className="send-button stop" onClick={onStop} title="停止" aria-label="停止">
          <svg viewBox="0 0 24 24" width="16" height="16" fill="currentColor" aria-hidden>
            <rect x="6" y="6" width="12" height="12" rx="2" />
          </svg>
        </button>
      ) : (
        <button
          className="send-button"
          onClick={send}
          disabled={!text.trim()}
          title="发送"
          aria-label="发送"
        >
          <svg
            viewBox="0 0 24 24"
            width="18"
            height="18"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
            aria-hidden
          >
            <path d="M12 19V5" />
            <path d="M5 12l7-7 7 7" />
          </svg>
        </button>
      )}
    </div>
  )
}
