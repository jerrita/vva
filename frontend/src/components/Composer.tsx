import { useEffect, useImperativeHandle, useRef, useState } from 'react'
import type { ClipboardEvent, DragEvent, KeyboardEvent, ReactNode, Ref } from 'react'
import { api } from '../api'
import type { FileAttachment, FsFindEntry, SkillEntry } from '../types'
import { IconArrowUp, IconPlus, IconStop, IconX } from './icons'

/** 外部写入输入框内容（回退后还原提示词） */
export interface ComposerHandle {
  set: (text: string) => void
}

/** 与 harness mention_at_cursor 对齐：开启边界后触发，空白/CJK 收尾标点结束 token。 */
const AT_PATTERN = /(?:^|[\s（【「『“‘《〈\(\[\{<])@([^\s@，．。；：！？）】」』”’》〉…]*)$/

type AtToken = { start: number; query: string }

export function Composer({
  running,
  onSend,
  onStop,
  controls,
  skills,
  workspace,
  contextRatio,
  contextText,
  onOpenStats,
  ref,
}: {
  running: boolean
  onSend: (text: string, files: FileAttachment[]) => void
  onStop: () => void
  controls?: ReactNode
  skills: SkillEntry[]
  workspace?: string
  /** 上下文占用比例（0–1）；缺省时不展示圆环 */
  contextRatio?: number
  contextText?: string
  onOpenStats?: () => void
  ref?: Ref<ComposerHandle>
}) {
  const [text, setText] = useState('')
  const [files, setFiles] = useState<FileAttachment[]>([])
  const [uploadedFiles, setUploadedFiles] = useState<{ filename: string; path: string }[]>([])
  const [uploading, setUploading] = useState(0)
  const [dragging, setDragging] = useState(false)
  const [skillIndex, setSkillIndex] = useState(0)
  const [paletteClosed, setPaletteClosed] = useState(false)
  // token 保存 @ 起点与光标前的查询；closed 仅屏蔽当前 token
  const [atToken, setAtToken] = useState<AtToken | null>(null)
  const [atClosed, setAtClosed] = useState(false)
  const [atResult, setAtResult] = useState<{
    token: AtToken
    workspace?: string
    entries: FsFindEntry[]
  } | null>(null)
  const [atIndex, setAtIndex] = useState(0)
  const textareaRef = useRef<HTMLTextAreaElement>(null)
  const fileInputRef = useRef<HTMLInputElement>(null)
  const selectedOptionRef = useRef<HTMLButtonElement>(null)
  const dragDepthRef = useRef(0)

  // /<skill_name> 快速选择：输入以 / 开头且不含空白时按名前缀过滤
  const query = text.trimStart()
  const skillMatches =
    query.startsWith('/') && !/\s/.test(query)
      ? skills.filter(skill => skill.name.toLowerCase().startsWith(query.slice(1).toLowerCase()))
      : []
  const paletteOpen = skillMatches.length > 0 && !paletteClosed
  const hasContent = text.trim() !== '' || files.length > 0 || uploadedFiles.length > 0
  const resetPalette = () => {
    setSkillIndex(0)
    setPaletteClosed(false)
  }
  const activeSkill = Math.min(skillIndex, Math.max(0, skillMatches.length - 1))

  // 候选绑定 token 与工作区；查询变化后的首帧即隐藏旧候选。
  const atEntries =
    atResult && atResult.token === atToken && atResult.workspace === workspace
      ? atResult.entries
      : []
  const atOpen = atToken != null && !atClosed && atEntries.length > 0
  const activeAt = Math.min(atIndex, Math.max(0, atEntries.length - 1))

  useEffect(() => {
    selectedOptionRef.current?.scrollIntoView({ block: 'nearest' })
  }, [skillIndex, atIndex])

  // token 变化（新 @ 或过滤词变化）时恢复弹层并复位选中；Esc 关闭仅对当前 token 生效
  useEffect(() => {
    setAtIndex(0)
    setAtClosed(false)
  }, [atToken])

  // 防抖拉取候选；关闭、换 token/工作区或卸载后丢弃响应。
  useEffect(() => {
    if (!atToken || atClosed) return
    let active = true
    const timer = setTimeout(() => {
      void api
        .fsFind(atToken.query, workspace, 12, true)
        .then(res => {
          if (active) setAtResult({ token: atToken, workspace, entries: res.entries })
        })
        .catch(() => {
          if (active) setAtResult(null)
        })
    }, 150)
    return () => {
      active = false
      clearTimeout(timer)
    }
  }, [atToken, atClosed, workspace])

  const detectAtToken = () => {
    const el = textareaRef.current
    if (!el) return
    const before = el.value.slice(0, el.selectionStart ?? el.value.length)
    const match = AT_PATTERN.exec(before)
    if (!match) {
      setAtToken(null)
      return
    }
    const token = { start: before.length - match[1].length - 1, query: match[1] }
    setAtToken(prev => (prev?.start === token.start && prev?.query === token.query ? prev : token))
  }

  /** 把光标处 @token 替换为 `@path `，光标落在插入末尾并保留后续文本 */
  const applyFile = (entry: FsFindEntry) => {
    const el = textareaRef.current
    if (!el) return
    const pos = el.selectionStart ?? el.value.length
    const before = el.value.slice(0, pos)
    const match = AT_PATTERN.exec(before)
    if (!match) {
      setAtToken(null)
      return
    }
    const head = before.slice(0, before.length - match[1].length - 1)
    const inserted = `@${entry.path} `
    setText(head + inserted + el.value.slice(pos))
    setAtToken(null)
    requestAnimationFrame(() => {
      const node = textareaRef.current
      if (!node) return
      const caret = head.length + inserted.length
      node.setSelectionRange(caret, caret)
      resize()
    })
  }

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
      resetPalette()
      setAtToken(null)
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
          const { path } = await api.uploadFile(file.name, data, workspace)
          setUploadedFiles(prev => [...prev, { filename: file.name, path }])
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

  const onDragEnter = (event: DragEvent<HTMLDivElement>) => {
    event.preventDefault()
    event.stopPropagation()
    dragDepthRef.current += 1
    if (event.dataTransfer.types.includes('Files')) setDragging(true)
  }

  const onDragLeave = (event: DragEvent<HTMLDivElement>) => {
    event.preventDefault()
    event.stopPropagation()
    dragDepthRef.current = Math.max(0, dragDepthRef.current - 1)
    if (dragDepthRef.current === 0) setDragging(false)
  }

  const onDrop = (event: DragEvent<HTMLDivElement>) => {
    event.preventDefault()
    event.stopPropagation()
    dragDepthRef.current = 0
    setDragging(false)
    const dropped = Array.from(event.dataTransfer.files)
    if (dropped.length > 0) void addFiles(dropped)
  }

  const send = () => {
    if (!hasContent || running || uploading > 0) return
    const prompt = [text.trim(), ...uploadedFiles.map(file => `文件已保存到 scratch：${file.path}`)]
      .filter(Boolean).join('\n\n')
    onSend(prompt, files)
    setText('')
    setFiles([])
    setUploadedFiles([])
    resetPalette()
    setAtToken(null)
    requestAnimationFrame(resize)
  }

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    // Shift+Enter 保留 textarea 的换行行为，并关闭当前补全。
    if (event.key === 'Enter' && event.shiftKey && !event.nativeEvent.isComposing) {
      setAtClosed(true)
      setPaletteClosed(true)
      return
    }
    if (atOpen && !event.nativeEvent.isComposing) {
      if (event.key === 'ArrowDown') {
        event.preventDefault()
        setAtIndex(index => (index + 1) % atEntries.length)
        return
      }
      if (event.key === 'ArrowUp') {
        event.preventDefault()
        setAtIndex(index => (index - 1 + atEntries.length) % atEntries.length)
        return
      }
      // Tab/Enter 仅插入 @path，不发送消息
      if (event.key === 'Tab' || event.key === 'Enter') {
        event.preventDefault()
        applyFile(atEntries[activeAt])
        return
      }
      if (event.key === 'Escape') {
        event.preventDefault()
        setAtClosed(true)
        return
      }
    }
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
    <div
      className={`composer${dragging ? ' dragging' : ''}`}
      onDragEnter={onDragEnter}
      onDragOver={event => {
        event.preventDefault()
        event.stopPropagation()
        event.dataTransfer.dropEffect = 'copy'
      }}
      onDragLeave={onDragLeave}
      onDrop={onDrop}
    >
      {dragging && <div className="composer-drop-hint">松开以上传文件</div>}
      {(files.length > 0 || uploadedFiles.length > 0) && (
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
          {uploadedFiles.map((file, index) => (
            <figure key={`uploaded-${index}`} className="attachment-chip">
              <span className="attachment-file" title={file.filename}>{file.filename}</span>
              <button
                onClick={() => setUploadedFiles(prev => prev.filter((_, i) => i !== index))}
                title="移除文件"
                aria-label={`移除文件 ${file.filename}`}
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
      {atOpen && (
        <div className="composer-menu" role="listbox" aria-label="文件和文件夹候选">
          {atEntries.map((entry, index) => {
            const name = entry.name + (entry.is_dir ? '/' : '')
            const dir = entry.path.slice(0, entry.path.length - name.length)
            return (
              <button
                key={entry.path}
                type="button"
                role="option"
                aria-selected={index === activeAt}
                ref={index === activeAt ? selectedOptionRef : undefined}
                className={`composer-option${index === activeAt ? ' selected' : ''}`}
                title={entry.path}
                onMouseDown={event => {
                  event.preventDefault()
                  applyFile(entry)
                }}
                onMouseEnter={() => setAtIndex(index)}
              >
                <span className="composer-option-name">{name}</span>
                <span className="composer-option-desc">{dir}</span>
              </button>
            )
          })}
        </div>
      )}
      <textarea
        ref={textareaRef}
        value={text}
        rows={1}
        placeholder={running ? '正在执行…' : '输入消息，/ 选择技能，@ 引用文件或文件夹，Enter 发送，Shift+Enter 换行'}
        onChange={event => {
          setText(event.target.value)
          resetPalette()
          detectAtToken()
          resize()
        }}
        onKeyDown={onKeyDown}
        onKeyUp={detectAtToken}
        onClick={detectAtToken}
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
        <div className="composer-actions">
        {contextRatio !== undefined && onOpenStats && (
          <ContextRing ratio={contextRatio} text={contextText ?? ''} onClick={onOpenStats} />
        )}
        {running ? (
          <button className="send-button stop" onClick={onStop} title="停止" aria-label="停止">
            <IconStop size={15} />
          </button>
        ) : (
          <button className="send-button" onClick={send} disabled={!hasContent || uploading > 0} title="发送" aria-label="发送">
            <IconArrowUp size={17} />
          </button>
        )}
        </div>
      </div>
    </div>
  )
}

const RING_R = 6.5
const RING_C = 2 * Math.PI * RING_R

/** 上下文占用圆环：点击打开用量弹层 */
function ContextRing({ ratio, text, onClick }: { ratio: number; text: string; onClick: () => void }) {
  const percent = Math.round(ratio * 100)
  const level = percent >= 90 ? ' danger' : percent >= 70 ? ' warning' : ''
  const label = `上下文 ${text}（${percent}%）`
  return (
    <button
      type="button"
      className={`ctx-ring${level}`}
      onClick={onClick}
      title={label}
      aria-label={label}
      data-stats-trigger
    >
      <svg width="16" height="16" viewBox="0 0 16 16" aria-hidden="true">
        <circle className="ctx-ring-track" cx="8" cy="8" r={RING_R} />
        <circle
          className="ctx-ring-fill"
          cx="8"
          cy="8"
          r={RING_R}
          strokeDasharray={RING_C}
          strokeDashoffset={RING_C * (1 - Math.max(ratio, 0.02))}
          transform="rotate(-90 8 8)"
        />
      </svg>
      <span className="ctx-ring-text">{percent}%</span>
    </button>
  )
}
