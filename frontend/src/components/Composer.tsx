import { useEffect, useImperativeHandle, useRef, useState } from 'react'
import type { ClipboardEvent, KeyboardEvent, ReactNode, Ref } from 'react'
import type { ImageAttachment } from '../types'

/** 外部写入输入框内容（回退后还原提示词） */
export interface ComposerHandle {
  set: (text: string) => void
}

export function Composer({
  running,
  onSend,
  onStop,
  controls,
  ref,
}: {
  running: boolean
  onSend: (text: string, images: ImageAttachment[]) => void
  onStop: () => void
  controls?: ReactNode
  ref?: Ref<ComposerHandle>
}) {
  const [text, setText] = useState('')
  const [images, setImages] = useState<ImageAttachment[]>([])
  const textareaRef = useRef<HTMLTextAreaElement>(null)

  const resize = () => {
    const el = textareaRef.current
    if (!el) return
    el.style.height = 'auto'
    el.style.height = `${Math.min(el.scrollHeight, 200)}px`
  }

  useImperativeHandle(ref, () => ({
    set: (value: string) => {
      setText(value)
      requestAnimationFrame(resize)
      textareaRef.current?.focus()
    },
  }))

  // 粘贴板中的图片转为 Base64 附件，非图片粘贴保持默认行为
  const onPaste = (event: ClipboardEvent<HTMLTextAreaElement>) => {
    const files = [...event.clipboardData.items]
      .filter(item => item.kind === 'file' && item.type.startsWith('image/'))
      .map(item => item.getAsFile())
      .filter((file): file is File => file !== null)
    if (files.length === 0) return
    event.preventDefault()
    for (const file of files) {
      const reader = new FileReader()
      reader.onload = () => {
        const result = typeof reader.result === 'string' ? reader.result : ''
        const data = result.slice(result.indexOf(',') + 1)
        if (data) setImages(prev => [...prev, { mime_type: file.type, data }])
      }
      reader.readAsDataURL(file)
    }
  }

  const send = () => {
    const trimmed = text.trim()
    if ((!trimmed && images.length === 0) || running) return
    onSend(trimmed, images)
    setText('')
    setImages([])
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
      {images.length > 0 && (
        <div className="composer-attachments">
          {images.map((image, index) => (
            <figure key={index} className="attachment-chip">
              <img src={`data:${image.mime_type};base64,${image.data}`} alt="" />
              <button
                onClick={() => setImages(prev => prev.filter((_, i) => i !== index))}
                title="移除图片"
                aria-label="移除图片"
              >
                <svg
                  viewBox="0 0 24 24"
                  width="11"
                  height="11"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="2.2"
                  strokeLinecap="round"
                  aria-hidden
                >
                  <path d="M18 6L6 18M6 6l12 12" />
                </svg>
              </button>
            </figure>
          ))}
        </div>
      )}
      <textarea
        ref={textareaRef}
        value={text}
        rows={1}
        placeholder={running ? '正在执行…' : '输入消息，Enter 发送，Shift+Enter 换行，可粘贴图片'}
        onChange={event => {
          setText(event.target.value)
          resize()
        }}
        onKeyDown={onKeyDown}
        onPaste={onPaste}
      />
      <div className="composer-foot">
        <div className="composer-tools">{controls}</div>
        {running ? (
          <button className="send-button stop" onClick={onStop} title="停止" aria-label="停止">
            <svg viewBox="0 0 24 24" width="15" height="15" fill="currentColor" aria-hidden>
              <rect x="6" y="6" width="12" height="12" rx="2" />
            </svg>
          </button>
        ) : (
          <button
            className="send-button"
            onClick={send}
            disabled={!text.trim() && images.length === 0}
            title="发送"
            aria-label="发送"
          >
            <svg
              viewBox="0 0 24 24"
              width="17"
              height="17"
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
    </div>
  )
}
