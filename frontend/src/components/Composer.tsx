import { useEffect, useImperativeHandle, useRef, useState } from 'react'
import type { ClipboardEvent, KeyboardEvent, ReactNode, Ref } from 'react'
import { api } from '../api'
import type { FileAttachment, SkillEntry } from '../types'
import { IconArrowUp, IconPlus, IconStop, IconX } from './icons'

/** 外部写入输入框内容（回退后还原提示词） */
export interface ComposerHandle {
  set: (text: string) => void
}

export function Composer({
  running,
  onSend,
  onStop,
  controls,
  skills,
  ref,
}: {
  running: boolean
  onSend: (text: string, files: FileAttachment[]) => void
  onStop: () => void
  controls?: ReactNode
  skills: SkillEntry[]
  ref?: Ref<ComposerHandle>
}) {
  const [text, setText] = useState('')
  const [files, setFiles] = useState<FileAttachment[]>([])
  const [uploading, setUploading] = useState(0)
  const [skillIndex, setSkillIndex] = useState(0)
  const [paletteClosed, setPaletteClosed] = useState(false)
  const textareaRef = useRef<HTMLTextAreaElement>(null)
  const fileInputRef = useRef<HTMLInputElement>(null)
  const selectedOptionRef = useRef<HTMLButtonElement>(null)

  // /<skill_name> 快速选择：输入以 / 开头且不含空白时按名前缀过滤
  const query = text.trimStart()
  const skillMatches =
    query.startsWith('/') && !/\s/.test(query)
      ? skills.filter(skill => skill.name.toLowerCase().startsWith(query.slice(1).toLowerCase()))
      : []
  const paletteOpen = skillMatches.length > 0 && !paletteClosed
  const activeSkill = Math.min(skillIndex, Math.max(0, skillMatches.length - 1))

  useEffect(() => {
    selectedOptionRef.current?.scrollIntoView({ block: 'nearest' })
  }, [skillIndex])

  const applySkill = (skill: SkillEntry) => {
    setText(`/${skill.name} `)
    setSkillIndex(0)
    requestAnimationFrame(resize)
  }

  const resize = () => {
    const el = textareaRef.current
    if (!el) return
    el.style.height = 'auto'
    el.style.height = `${Math.min(el.scrollHeight, 200)}px`
  }

  useImperativeHandle(ref, () => ({
    set: (value: string) => {
      setText(value)
      setSkillIndex(0)
      setPaletteClosed(false)
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
    setSkillIndex(0)
    setPaletteClosed(false)
    requestAnimationFrame(resize)
  }

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (paletteOpen && !event.nativeEvent.isComposing) {
      if (event.key === 'ArrowDown') {
        event.preventDefault()
        setSkillIndex(index => (index + 1) % skillMatches.length)
        return
      }
      if (event.key === 'ArrowUp') {
        event.preventDefault()
        setSkillIndex(index => (index - 1 + skillMatches.length) % skillMatches.length)
        return
      }
      // Tab/Enter 仅补全技能名，任务描述待用户继续输入后发送
      if (event.key === 'Tab' || event.key === 'Enter') {
        event.preventDefault()
        applySkill(skillMatches[activeSkill])
        return
      }
      if (event.key === 'Escape') {
        event.preventDefault()
        setPaletteClosed(true)
        return
      }
    }
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
                <IconX size={11} strokeWidth={2.2} />
              </button>
            </figure>
          ))}
        </div>
      )}
      {paletteOpen && (
        <div className="composer-menu" role="listbox" aria-label="技能候选">
          {skillMatches.map((skill, index) => (
            <button
              key={skill.name}
              type="button"
              role="option"
              aria-selected={index === activeSkill}
              ref={index === activeSkill ? selectedOptionRef : undefined}
              className={`composer-option${index === activeSkill ? ' selected' : ''}`}
              title={`${skill.description}\n${skill.path}`}
              onMouseDown={event => {
                event.preventDefault()
                applySkill(skill)
              }}
              onMouseEnter={() => setSkillIndex(index)}
            >
              <span className="composer-option-name">/{skill.name}</span>
              <span className="composer-option-desc">{skill.description}</span>
            </button>
          ))}
        </div>
      )}
      <textarea
        ref={textareaRef}
        value={text}
        rows={1}
        placeholder={running ? '正在执行…' : '输入消息，/ 选择技能，Enter 发送，Shift+Enter 换行'}
        onChange={event => {
          setText(event.target.value)
          setSkillIndex(0)
          setPaletteClosed(false)
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
            <IconPlus size={17} />
          </button>
          {controls}
        </div>
        {running ? (
          <button className="send-button stop" onClick={onStop} title="停止" aria-label="停止">
            <IconStop size={15} />
          </button>
        ) : (
          <button className="send-button" onClick={send} disabled={(!text.trim() && files.length === 0) || uploading > 0} title="发送" aria-label="发送">
            <IconArrowUp size={17} />
          </button>
        )}
      </div>
    </div>
  )
}
