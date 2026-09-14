import { useEffect, useMemo, useRef, useState } from 'react'
import type { PointerEvent as ReactPointerEvent } from 'react'
import hljs from 'highlight.js/lib/common'
import { api } from '../api'
import type { ContentPart, RelatedFile, Session, SessionFileResponse } from '../types'
import { CopyButton } from './CopyButton'
import { Markdown } from './Markdown'

export const FILE_SIDEBAR_MIN = 240
export const FILE_SIDEBAR_MAX = 760

export const clampFileSidebar = (width: number) =>
  Math.min(FILE_SIDEBAR_MAX, Math.max(FILE_SIDEBAR_MIN, width))

function cleanPath(raw: string): string {
  let p = raw.trim()
  while ((p.startsWith('"') && p.endsWith('"')) || (p.startsWith("'") && p.endsWith("'"))) {
    p = p.slice(1, -1).trim()
  }
  return p
}

function fmtBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
}

const LANGUAGE_BY_EXTENSION: Record<string, string> = {
  bash: 'bash',
  c: 'c',
  cc: 'cpp',
  cpp: 'cpp',
  cs: 'csharp',
  css: 'css',
  go: 'go',
  h: 'c',
  hpp: 'cpp',
  html: 'xml',
  java: 'java',
  js: 'javascript',
  json: 'json',
  jsx: 'javascript',
  md: 'markdown',
  mjs: 'javascript',
  py: 'python',
  rb: 'ruby',
  rs: 'rust',
  scss: 'scss',
  sh: 'bash',
  sql: 'sql',
  swift: 'swift',
  ts: 'typescript',
  tsx: 'typescript',
  toml: 'ini',
  xml: 'xml',
  yaml: 'yaml',
  yml: 'yaml',
}

function languageForPath(path: string | null): string | null {
  if (!path) return null
  const name = path.split('/').pop()?.toLowerCase() ?? ''
  if (name === 'dockerfile') return 'dockerfile'
  const extension = name.split('.').pop()
  return extension ? (LANGUAGE_BY_EXTENSION[extension] ?? null) : null
}

function splitHighlightedLines(html: string): string[] {
  const lines = ['']
  const openTags: string[] = []
  const tokens = html.match(/<[^>]+>|[^<]+/g) ?? []

  for (const token of tokens) {
    if (token.startsWith('<span')) {
      lines[lines.length - 1] += token
      openTags.push(token)
      continue
    }
    if (token === '</span>') {
      lines[lines.length - 1] += token
      openTags.pop()
      continue
    }

    const parts = token.split('\n')
    lines[lines.length - 1] += parts[0]
    for (const part of parts.slice(1)) {
      lines[lines.length - 1] += openTags.map(() => '</span>').reverse().join('')
      lines.push(openTags.join('') + part)
    }
  }

  lines[lines.length - 1] += openTags.map(() => '</span>').reverse().join('')
  return lines
}

/**
 * 从会话轮次与当前草稿中提取涉及的文件（Read / Write / Edit / 附件）
 */
export function extractRelatedFiles(
  session: Session | null,
  draft?: ContentPart[],
): RelatedFile[] {
  if (!session) return []
  const map = new Map<
    string,
    {
      path: string
      name: string
      actions: Set<'read' | 'write' | 'edit' | 'attachment'>
      lastAction: 'read' | 'write' | 'edit' | 'attachment'
      turnIndex: number
    }
  >()

  const record = (
    path: string,
    action: 'read' | 'write' | 'edit' | 'attachment',
    turnIndex: number,
  ) => {
    const cleaned = cleanPath(path)
    if (!cleaned) return
    const name = cleaned.split('/').filter(Boolean).pop() || cleaned
    const existing = map.get(cleaned)
    if (existing) {
      existing.actions.add(action)
      existing.lastAction = action
      existing.turnIndex = Math.max(existing.turnIndex, turnIndex)
    } else {
      map.set(cleaned, {
        path: cleaned,
        name,
        actions: new Set([action]),
        lastAction: action,
        turnIndex,
      })
    }
  }

  const processParts = (parts: ContentPart[], turnIndex: number) => {
    for (const part of parts) {
      if (part.type === 'tool_call') {
        if (part.name === 'Read' || part.name === 'Write' || part.name === 'Edit') {
          try {
            const args = JSON.parse(part.arguments)
            if (args && typeof args === 'object' && typeof args.path === 'string') {
              record(
                args.path,
                part.name.toLowerCase() as 'read' | 'write' | 'edit',
                turnIndex,
              )
            }
          } catch {}
        }
      } else if (part.type === 'file') {
        if (part.filename) {
          record(part.filename, 'attachment', turnIndex)
        }
      }
    }
  }

  session.turns.forEach((turn, turnIndex) => {
    for (const msg of turn.messages) {
      processParts(msg.parts, turnIndex)
    }
  })

  if (draft && draft.length > 0) {
    processParts(draft, session.turns.length)
  }

  const files = [...map.values()].map(item => ({
    path: item.path,
    name: item.name,
    actions: [...item.actions],
    lastAction: item.lastAction,
    turnIndex: item.turnIndex,
  }))

  files.sort((a, b) => b.turnIndex - a.turnIndex || a.name.localeCompare(b.name))
  return files
}

interface FileSidebarProps {
  sessionId: string | null
  session: Session | null
  draft?: ContentPart[]
  activePath: string | null
  open: boolean
  width: number
  onSelectPath: (path: string | null) => void
  onClose: () => void
  onResizeStart: (event: ReactPointerEvent<HTMLDivElement>) => void
  onResizeMove: (event: ReactPointerEvent<HTMLDivElement>) => void
  onResizeEnd: (event: ReactPointerEvent<HTMLDivElement>) => void
}

export function FileSidebar({
  sessionId,
  session,
  draft,
  activePath,
  open,
  width,
  onSelectPath,
  onClose,
  onResizeStart,
  onResizeMove,
  onResizeEnd,
}: FileSidebarProps) {
  const [filter, setFilter] = useState('')
  const [fileData, setFileData] = useState<SessionFileResponse | null>(null)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [viewMode, setViewMode] = useState<'render' | 'source'>('render')

  const files = useMemo(() => extractRelatedFiles(session, draft), [session, draft])

  const filteredFiles = useMemo(() => {
    if (!filter.trim()) return files
    const query = filter.toLowerCase().trim()
    return files.filter(
      f => f.name.toLowerCase().includes(query) || f.path.toLowerCase().includes(query),
    )
  }, [files, filter])

  // 当 activePath 改变时加载文件内容
  useEffect(() => {
    if (!sessionId || !activePath) {
      setFileData(null)
      setError(null)
      setLoading(false)
      return
    }
    let cancelled = false
    setLoading(true)
    setError(null)
    api
      .sessionFile(sessionId, activePath)
      .then(res => {
        if (!cancelled) {
          setFileData(res)
          setLoading(false)
        }
      })
      .catch(err => {
        if (!cancelled) {
          setError(err instanceof Error ? err.message : String(err))
          setLoading(false)
        }
      })
    return () => {
      cancelled = true
    }
  }, [sessionId, activePath])

  const lines = useMemo(() => {
    if (!fileData || fileData.is_binary) return []
    return fileData.content.split('\n')
  }, [fileData])

  const highlightedLines = useMemo(() => {
    if (!fileData || fileData.is_binary) return null
    const language = languageForPath(activePath)
    if (!language || !hljs.getLanguage(language)) return null
    return splitHighlightedLines(hljs.highlight(fileData.content, { language }).value)
  }, [activePath, fileData])

  const isMarkdown = activePath ? /\.md$/i.test(activePath) : false

  const activeFileInfo = useMemo(
    () => (activePath ? files.find(f => f.path === activePath) : null),
    [files, activePath],
  )

  const scrollerRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (scrollerRef.current) {
      scrollerRef.current.scrollTop = 0
    }
  }, [activePath])

  if (!open) return null

  return (
    <aside
      className="file-sidebar"
      style={{ width: `${clampFileSidebar(width)}px` }}
      aria-label="相关文件预览"
    >
      <div
        className="file-sidebar-resize"
        onPointerDown={onResizeStart}
        onPointerMove={onResizeMove}
        onPointerUp={onResizeEnd}
        onPointerCancel={onResizeEnd}
        title="拖拽调节宽度"
      />

      <div className="file-sidebar-head">
        {activePath ? (
          <div className="file-sidebar-nav">
            <button
              className="icon-ghost file-nav-back"
              onClick={() => onSelectPath(null)}
              title="返回文件列表"
              aria-label="返回文件列表"
            >
              <svg
                viewBox="0 0 24 24"
                width="16"
                height="16"
                fill="none"
                stroke="currentColor"
                strokeWidth="2"
                strokeLinecap="round"
                strokeLinejoin="round"
                aria-hidden
              >
                <path d="M19 12H5" />
                <path d="M12 19l-7-7 7-7" />
              </svg>
            </button>
            <div className="file-nav-title" title={activePath}>
              <span className="file-nav-name">{activeFileInfo?.name || activePath}</span>
              <span className="file-nav-path">{activePath}</span>
            </div>
          </div>
        ) : (
          <div className="file-sidebar-title-row">
            <span className="file-sidebar-title">相关文件</span>
            {files.length > 0 && (
              <span className="file-count-badge">{files.length}</span>
            )}
          </div>
        )}

        <div className="file-sidebar-head-actions">
          <button
            className="icon-ghost file-sidebar-close"
            onClick={onClose}
            title="关闭侧边栏"
            aria-label="关闭侧边栏"
          >
            <svg
              viewBox="0 0 24 24"
              width="16"
              height="16"
              fill="none"
              stroke="currentColor"
              strokeWidth="2"
              strokeLinecap="round"
              strokeLinejoin="round"
              aria-hidden
            >
              <line x1="18" y1="6" x2="6" y2="18" />
              <line x1="6" y1="6" x2="18" y2="18" />
            </svg>
          </button>
        </div>
      </div>

      {!activePath ? (
        <div className="file-sidebar-list-container">
          {files.length > 4 && (
            <div className="file-sidebar-filter-wrap">
              <input
                type="search"
                className="file-sidebar-filter"
                placeholder="筛选相关文件…"
                value={filter}
                onChange={e => setFilter(e.target.value)}
              />
            </div>
          )}

          <div className="file-sidebar-list">
            {filteredFiles.length === 0 ? (
              <div className="file-empty-state">
                <svg
                  viewBox="0 0 24 24"
                  width="36"
                  height="36"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="1.4"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  aria-hidden
                >
                  <path d="M13 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V9z" />
                  <polyline points="13 2 13 9 20 9" />
                </svg>
                <div className="file-empty-title">
                  {filter ? '未找到匹配文件' : '暂无涉及文件'}
                </div>
                <div className="file-empty-hint">
                  {filter ? '请尝试其他关键词' : '模型执行 Read、Edit、Write 操作后将自动在此汇总'}
                </div>
              </div>
            ) : (
              filteredFiles.map(file => {
                const parentDir =
                  file.path.includes('/')
                    ? file.path.slice(0, file.path.lastIndexOf('/'))
                    : ''
                return (
                  <button
                    key={file.path}
                    className="file-item"
                    onClick={() => onSelectPath(file.path)}
                    title={file.path}
                  >
                    <div className="file-item-icon">
                      <svg
                        viewBox="0 0 24 24"
                        width="16"
                        height="16"
                        fill="none"
                        stroke="currentColor"
                        strokeWidth="1.8"
                        strokeLinecap="round"
                        strokeLinejoin="round"
                        aria-hidden
                      >
                        <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
                        <polyline points="14 2 14 8 20 8" />
                        <line x1="16" y1="13" x2="8" y2="13" />
                        <line x1="16" y1="17" x2="8" y2="17" />
                        <line x1="10" y1="9" x2="8" y2="9" />
                      </svg>
                    </div>
                    <div className="file-item-text">
                      <span className="file-item-name">{file.name}</span>
                      {parentDir && <span className="file-item-dir">{parentDir}</span>}
                    </div>
                    <div className="file-item-badges">
                      {file.actions.map(action => (
                        <span key={action} className={`file-badge file-badge-${action}`}>
                          {action}
                        </span>
                      ))}
                    </div>
                  </button>
                )
              })
            )}
          </div>
        </div>
      ) : (
        <div className="file-preview-container">
          <div className="file-preview-meta-bar">
            <div className="file-meta-tags">
              {fileData && (
                <>
                  {!fileData.is_binary && (
                    <span className="file-meta-item">{lines.length} 行</span>
                  )}
                  <span className="file-meta-item">{fmtBytes(fileData.size)}</span>
                </>
              )}
            </div>

            <div className="file-meta-actions">
              {isMarkdown && !loading && !error && fileData && !fileData.is_binary && (
                <div className="file-view-toggle">
                  <button
                    className={`file-toggle-btn${viewMode === 'render' ? ' active' : ''}`}
                    onClick={() => setViewMode('render')}
                  >
                    预览
                  </button>
                  <button
                    className={`file-toggle-btn${viewMode === 'source' ? ' active' : ''}`}
                    onClick={() => setViewMode('source')}
                  >
                    源码
                  </button>
                </div>
              )}
              {fileData && !fileData.is_binary && <CopyButton text={fileData.content} />}
            </div>
          </div>

          <div className="file-preview-body" ref={scrollerRef}>
            {loading && (
              <div className="file-preview-status loading">
                <span className="file-loading-spinner" />
                <span>加载文件中…</span>
              </div>
            )}

            {error && (
              <div className="file-preview-status error">
                <span className="file-status-icon">✕</span>
                <span>{error}</span>
              </div>
            )}

            {!loading && !error && fileData && (
              <>
                {fileData.is_binary ? (
                  fileData.mime_type?.startsWith('image/') ? (
                    <div className="file-preview-image-wrap">
                      <img
                        src={`data:${fileData.mime_type};base64,${fileData.content}`}
                        alt={fileData.name}
                        className="file-preview-image"
                      />
                    </div>
                  ) : (
                    <div className="file-preview-status binary">
                      <svg
                        viewBox="0 0 24 24"
                        width="32"
                        height="32"
                        fill="none"
                        stroke="currentColor"
                        strokeWidth="1.8"
                        strokeLinecap="round"
                        strokeLinejoin="round"
                        aria-hidden
                      >
                        <rect x="2" y="2" width="20" height="20" rx="2" />
                        <path d="M12 18h.01" />
                        <path d="M8 6h8" />
                        <path d="M8 10h8" />
                        <path d="M8 14h8" />
                      </svg>
                      <span>二进制文件 ({fileData.mime_type || '未知类型'})</span>
                      <span className="file-status-sub">不支持在线文本预览</span>
                    </div>
                  )
                ) : isMarkdown && viewMode === 'render' ? (
                  <div className="file-preview-markdown">
                    <Markdown text={fileData.content} />
                  </div>
                ) : (
                  <div className="file-preview-code">
                    <div className="file-code-gutter" aria-hidden>
                      {lines.map((_, i) => (
                        <span key={i} className="file-code-lineno">
                          {i + 1}
                        </span>
                      ))}
                    </div>
                    <div className="file-code-content">
                      {lines.map((line, i) => (
                        <div key={i} className="file-code-line">
                          {highlightedLines ? (
                            <span
                              dangerouslySetInnerHTML={{
                                __html: highlightedLines[i] || ' ',
                              }}
                            />
                          ) : (
                            line || ' '
                          )}
                        </div>
                      ))}
                    </div>
                  </div>
                )}
              </>
            )}
          </div>
        </div>
      )}
    </aside>
  )
}
