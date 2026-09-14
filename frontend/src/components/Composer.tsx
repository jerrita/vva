import { useEffect, useImperativeHandle, useRef, useState } from 'react'
import type { ClipboardEvent, KeyboardEvent, ReactNode, Ref } from 'react'
import { api } from '../api'
import type { FileAttachment } from '../types'

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
  onSend: (text: string, files: FileAttachment[]) => void
  onStop: () => void
  controls?: ReactNode
  ref?: Ref<ComposerHandle>
}) {
  const [text, setText] = useState('')
  const [files, setFiles] = useState<FileAttachment[]>([])
  const [uploading, setUploading] = useState(0)
  const textareaRef = useRef<HTMLTextAreaElement>(null)
  const fileInputRef = useRef<HTMLInputElement>(null)

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

  const readData = (file: File) =>
    new Promise<string>((resolve, reject) => {
      const reader = new FileReader()
      reader.onload = () => {
        const result = typeof reader.result === 'string' ? reader.result : ''
        resolve(result.slice(result.indexOf(',') + 1))
      }
      reader.onerror = () => reject(reader.error ?? new Error('读取文件失败'))
      reader.readAsDataURL(file)
    })

  const addFiles = async (selected: File[]) => {
    for (const file of selected) {
      const native = file.type === 'application/pdf' || ['image/png', 'image/jpeg', 'image/gif', 'image/webp'].includes(file.type)
      try {
        const data = await readData(file)
        if (!data) continue
        if (native) {
          setFiles(prev => [...prev, { filename: file.name, mime_type: file.type, data }])
        } else {
          setUploading(count => count + 1)
          const { path } = await api.uploadFile(file.name, data)
          setText(prev => `${prev}${prev.trim() ? '\n\n' : ''}文件已保存到 scratch：${path}`)
        }
      } catch {
        setText(prev => `${prev}${prev.trim() ? '\n\n' : ''}文件上传失败：${file.name}`)
      } finally {
        if (!native) setUploading(count => Math.max(0, count - 1))
      }
    }
  }

  const onPaste = (event: ClipboardEvent<HTMLTextAreaElement>) => {
    const pasted = [...event.clipboardData.items]
      .filter(item => item.kind === 'file' && item.type.startsWith('image/'))
      .map(item => item.getAsFile())
      .filter((file): file is File => file !== null)
    if (pasted.length === 0) return
    event.preventDefault()
    void addFiles(pasted)
  }

  const send = () => {
    const trimmed = text.trim()
    if ((!trimmed && files.length === 0) || running || uploading > 0) return
    onSend(trimmed, files)
    setText('')
    setFiles([])
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
      {files.length > 0 && (
        <div className="composer-attachments">
          {files.map((file, index) => (
            <figure key={`${file.filename}-${index}`} className="attachment-chip">
              {file.mime_type.startsWith('image/') ? (
                <img src={`data:${file.mime_type};base64,${file.data}`} alt={file.filename} />
              ) : (
                <span className="attachment-file">{file.filename}</span>
              )}
              <button
                onClick={() => setFiles(prev => prev.filter((_, i) => i !== index))}
                title="移除文件"
                aria-label="移除文件"
              >
                <svg viewBox="0 0 24 24" width="11" height="11" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" aria-hidden>
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
        placeholder={running ? '正在执行…' : '输入消息，Enter 发送，Shift+Enter 换行'}
        onChange={event => {
          setText(event.target.value)
          resize()
        }}
        onKeyDown={onKeyDown}
        onPaste={onPaste}
      />
      <div className="composer-foot">
        <div className="composer-tools">
          <input
            ref={fileInputRef}
            type="file"
            accept="*/*"
            multiple
            hidden
            onChange={event => {
              void addFiles(Array.from(event.target.files ?? []))
              event.target.value = ''
            }}
          />
          <button className="icon-ghost" onClick={() => fileInputRef.current?.click()} title="添加文件" aria-label="添加文件">
            <svg viewBox="0 0 24 24" width="17" height="17" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" aria-hidden>
              <path d="M12 5v14M5 12h14" />
            </svg>
          </button>
          {controls}
        </div>
        {running ? (
          <button className="send-button stop" onClick={onStop} title="停止" aria-label="停止">
            <svg viewBox="0 0 24 24" width="15" height="15" fill="currentColor" aria-hidden><rect x="6" y="6" width="12" height="12" rx="2" /></svg>
          </button>
        ) : (
          <button className="send-button" onClick={send} disabled={(!text.trim() && files.length === 0) || uploading > 0} title="发送" aria-label="发送">
            <svg viewBox="0 0 24 24" width="17" height="17" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden>
              <path d="M12 19V5" /><path d="M5 12l7-7 7 7" />
            </svg>
          </button>
        )}
      </div>
    </div>
  )
}
