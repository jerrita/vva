import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import hljs from 'highlight.js/lib/common'
import { api, toError } from '../api'
import type { ContentPart, FsEntry, RelatedFile, Session, SessionFileResponse } from '../types'
import { CopyButton } from './CopyButton'
import { Markdown } from './Markdown'
import {
  IconArrowLeft,
  IconChevronDown,
  IconChevronRight,
  IconDownload,
  IconFile,
  IconFolderClosed,
  IconFolderOpen,
  IconX,
} from './icons'

export const FILE_SIDEBAR_MIN = 240
export const FILE_SIDEBAR_MAX = 1000

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
      existing.turnIndex = Math.max(existing.turnIndex, turnIndex)
    } else {
      map.set(cleaned, {
        path: cleaned,
        name,
        actions: new Set([action]),
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
    turnIndex: item.turnIndex,
  }))

  files.sort((a, b) => b.turnIndex - a.turnIndex || a.name.localeCompare(b.name))
  return files
}

interface TreeNodeProps {
  entry: FsEntry
  depth: number
  activePath: string | null
  expandedDirs: Set<string>
  dirChildren: Record<string, FsEntry[]>
  dirLoading: Record<string, boolean>
  onToggleDir: (path: string) => void
  onSelectFile: (path: string) => void
}

function TreeNode({
  entry,
  depth,
  activePath,
  expandedDirs,
  dirChildren,
  dirLoading,
  onToggleDir,
  onSelectFile,
}: TreeNodeProps) {
  const isExpanded = expandedDirs.has(entry.path)
  const isLoading = !!dirLoading[entry.path]
  const children = dirChildren[entry.path] || []
  const isActive = entry.path === activePath

  if (entry.is_dir) {
    return (
      <div className="file-tree-node">
        <button
          type="button"
          className="file-tree-row dir"
          style={{ paddingLeft: `${depth * 14 + 8}px` }}
          onClick={() => onToggleDir(entry.path)}
          title={entry.path}
        >
          <span className="file-tree-chevron">
            {isExpanded ? <IconChevronDown size={14} /> : <IconChevronRight size={14} />}
          </span>
          <span className="file-tree-icon dir">
            {isExpanded ? <IconFolderOpen size={15} /> : <IconFolderClosed size={15} />}
          </span>
          <span className="file-tree-name">{entry.name}</span>
          {isLoading && <span className="file-loading-spinner small" />}
        </button>
        {isExpanded && (
          <div className="file-tree-children">
            {children.length === 0 && !isLoading ? (
              <div
                className="file-tree-empty"
                style={{ paddingLeft: `${(depth + 1) * 14 + 22}px` }}
              >
                空目录
              </div>
            ) : (
              children.map(child => (
                <TreeNode
                  key={child.path}
                  entry={child}
                  depth={depth + 1}
                  activePath={activePath}
                  expandedDirs={expandedDirs}
                  dirChildren={dirChildren}
                  dirLoading={dirLoading}
                  onToggleDir={onToggleDir}
                  onSelectFile={onSelectFile}
                />
              ))
            )}
          </div>
        )}
      </div>
    )
  }

  return (
    <button
      type="button"
      className={`file-tree-row file${isActive ? ' active' : ''}`}
      style={{ paddingLeft: `${depth * 14 + 22}px` }}
      onClick={() => onSelectFile(entry.path)}
      title={entry.path}
    >
      <span className="file-tree-icon file">
        <IconFile size={15} />
      </span>
      <span className="file-tree-name">{entry.name}</span>
      <span className="file-tree-size">{fmtBytes(entry.size)}</span>
    </button>
  )
}

interface FileSidebarProps {
  sessionId: string | null
  workspace?: string | null
  files: RelatedFile[]
  activePath: string | null
  open: boolean
  width: number
  onSelectPath: (path: string | null) => void
  onClose: () => void
}

export function FileSidebar({
  sessionId,
  workspace,
  files,
  activePath,
  open,
  width,
  onSelectPath,
  onClose,
}: FileSidebarProps) {
  const [activeTab, setActiveTab] = useState<'tree' | 'related'>('tree')
  const [filter, setFilter] = useState('')
  const [treeFilter, setTreeFilter] = useState('')
  const [rootEntries, setRootEntries] = useState<FsEntry[]>([])
  const [rootLoading, setRootLoading] = useState(false)
  const [rootError, setRootError] = useState<string | null>(null)
  const [dirChildren, setDirChildren] = useState<Record<string, FsEntry[]>>({})
  const [dirLoading, setDirLoading] = useState<Record<string, boolean>>({})
  const [expandedDirs, setExpandedDirs] = useState<Set<string>>(new Set())
  const [fileData, setFileData] = useState<SessionFileResponse | null>(null)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [viewMode, setViewMode] = useState<'render' | 'source'>('render')
  const [isMobile, setIsMobile] = useState(false)

  useEffect(() => {
    const media = window.matchMedia('(max-width: 820px)')
    const sync = () => setIsMobile(media.matches)
    sync()
    media.addEventListener('change', sync)
    return () => media.removeEventListener('change', sync)
  }, [])

  // 加载工程根目录条目
  useEffect(() => {
    if (!open) return
    let cancelled = false
    setRootLoading(true)
    setRootError(null)
    api
      .fsEntries(workspace || undefined)
      .then(res => {
        if (!cancelled) {
          setRootEntries(res.entries)
          setRootLoading(false)
        }
      })
      .catch(err => {
        if (!cancelled) {
          setRootError(toError(err))
          setRootLoading(false)
        }
      })
    return () => {
      cancelled = true
    }
  }, [open, workspace])

  // 展开或折叠目录
  const onToggleDir = useCallback(
    (path: string) => {
      setExpandedDirs(prev => {
        const next = new Set(prev)
        if (next.has(path)) {
          next.delete(path)
          return next
        }
        next.add(path)
        return next
      })

      setDirChildren(current => {
        if (!current[path]) {
          setDirLoading(loading => ({ ...loading, [path]: true }))
          api
            .fsEntries(workspace || undefined, path)
            .then(res => {
              setDirChildren(prev => ({ ...prev, [path]: res.entries }))
            })
            .catch(err => {
              console.error('Failed to load directory entries:', err)
            })
            .finally(() => {
              setDirLoading(loading => ({ ...loading, [path]: false }))
            })
        }
        return current
      })
    },
    [workspace],
  )

  // 树过滤匹配条目（含根与已缓存的子目录）
  const filteredTreeEntries = useMemo(() => {
    if (!treeFilter.trim()) return null
    const query = treeFilter.toLowerCase().trim()
    const all = new Map<string, FsEntry>()
    for (const e of rootEntries) {
      all.set(e.path, e)
    }
    for (const children of Object.values(dirChildren)) {
      for (const e of children) {
        all.set(e.path, e)
      }
    }
    return [...all.values()].filter(
      e => e.name.toLowerCase().includes(query) || e.path.toLowerCase().includes(query),
    )
  }, [rootEntries, dirChildren, treeFilter])

  const filteredFiles = useMemo(() => {
    if (!filter.trim()) return files
    const query = filter.toLowerCase().trim()
    return files.filter(
      f => f.name.toLowerCase().includes(query) || f.path.toLowerCase().includes(query),
    )
  }, [files, filter])

  // 当 activePath 改变时加载文件内容
  useEffect(() => {
    if (!activePath) {
      setFileData(null)
      setError(null)
      setLoading(false)
      return
    }
    let cancelled = false
    setLoading(true)
    setError(null)

    const fetcher = sessionId
      ? api.sessionFile(sessionId, activePath)
      : api.fsFile(activePath, workspace || undefined)

    fetcher
      .then(res => {
        if (!cancelled) {
          setFileData(res)
          setLoading(false)
        }
      })
      .catch(err => {
        if (!cancelled) {
          setError(toError(err))
          setLoading(false)
        }
      })
    return () => {
      cancelled = true
    }
  }, [sessionId, workspace, activePath])

  // 当 activePath 改变时，自动展开其所有上级目录并预取条目（Reveal in Tree）
  useEffect(() => {
    if (!activePath) return
    const parts = activePath.split('/').filter(Boolean)
    if (parts.length <= 1) return

    const ancestors: string[] = []
    let cur = ''
    for (let i = 0; i < parts.length - 1; i++) {
      cur = cur ? `${cur}/${parts[i]}` : parts[i]
      ancestors.push(cur)
    }

    setExpandedDirs(prev => {
      let changed = false
      const next = new Set(prev)
      for (const anc of ancestors) {
        if (!next.has(anc)) {
          next.add(anc)
          changed = true
        }
      }
      return changed ? next : prev
    })

    for (const anc of ancestors) {
      setDirChildren(current => {
        if (!current[anc]) {
          setDirLoading(loading => ({ ...loading, [anc]: true }))
          api
            .fsEntries(workspace || undefined, anc)
            .then(res => {
              setDirChildren(prev => ({ ...prev, [anc]: res.entries }))
            })
            .catch(err => {
              console.error('Failed to reveal directory entries:', err)
            })
            .finally(() => {
              setDirLoading(loading => ({ ...loading, [anc]: false }))
            })
        }
        return current
      })
    }
  }, [activePath, workspace])

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

  // 小屏采用“列表 → 预览”两步流，确保文件内容和下载操作都有足够空间。
  const isDualPane = Boolean(activePath && width >= 520 && !isMobile)

  const renderNavList = () => {
    if (activeTab === 'tree') {
      return (
        <>
          <div className="file-sidebar-filter-wrap">
            <input
              type="search"
              className="file-sidebar-filter"
              placeholder="筛选工程文件…"
              value={treeFilter}
              onChange={e => setTreeFilter(e.target.value)}
            />
          </div>

          <div className="file-sidebar-list">
            {rootLoading ? (
              <div className="file-tree-status">
                <span className="file-loading-spinner small" />
                <span>加载工程目录中…</span>
              </div>
            ) : rootError ? (
              <div className="file-tree-status error">
                <span>{rootError}</span>
              </div>
            ) : filteredTreeEntries ? (
              filteredTreeEntries.length === 0 ? (
                <div className="file-empty-state">
                  <IconFile size={36} strokeWidth={1.4} />
                  <div className="file-empty-title">未找到匹配文件</div>
                </div>
              ) : (
                filteredTreeEntries.map(file => {
                  const parentDir = file.path.includes('/')
                    ? file.path.slice(0, file.path.lastIndexOf('/'))
                    : ''
                  const isActive = file.path === activePath
                  return (
                    <button
                      key={file.path}
                      type="button"
                      className={`file-item${isActive ? ' active' : ''}`}
                      onClick={() => {
                        if (file.is_dir) {
                          setExpandedDirs(prev => new Set([...prev, file.path]))
                          setTreeFilter('')
                        } else {
                          onSelectPath(file.path)
                        }
                      }}
                      title={file.path}
                    >
                      <div className="file-item-icon">
                        {file.is_dir ? <IconFolderClosed size={16} /> : <IconFile size={16} />}
                      </div>
                      <div className="file-item-text">
                        <span className="file-item-name">{file.name}</span>
                        {parentDir && <span className="file-item-dir">{parentDir}</span>}
                      </div>
                      {!file.is_dir && (
                        <span className="file-tree-size">{fmtBytes(file.size)}</span>
                      )}
                    </button>
                  )
                })
              )
            ) : rootEntries.length === 0 ? (
              <div className="file-empty-state">
                <IconFolderClosed size={36} strokeWidth={1.4} />
                <div className="file-empty-title">工程目录为空</div>
              </div>
            ) : (
              <div className="file-tree-view">
                {rootEntries.map(entry => (
                  <TreeNode
                    key={entry.path}
                    entry={entry}
                    depth={0}
                    activePath={activePath}
                    expandedDirs={expandedDirs}
                    dirChildren={dirChildren}
                    dirLoading={dirLoading}
                    onToggleDir={onToggleDir}
                    onSelectFile={onSelectPath}
                  />
                ))}
              </div>
            )}
          </div>
        </>
      )
    }

    return (
      <>
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
              <IconFile size={36} strokeWidth={1.4} />
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
              const isActive = file.path === activePath
              return (
                <button
                  key={file.path}
                  type="button"
                  className={`file-item${isActive ? ' active' : ''}`}
                  onClick={() => onSelectPath(file.path)}
                  title={file.path}
                >
                  <div className="file-item-icon">
                    <IconFile size={16} />
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
      </>
    )
  }

  const renderPreviewBody = () => {
    if (loading) {
      return (
        <div className="file-preview-status loading">
          <span className="file-loading-spinner" />
          <span>加载文件中…</span>
        </div>
      )
    }

    if (error) {
      return (
        <div className="file-preview-status error">
          <span className="file-status-icon">✕</span>
          <span>{error}</span>
        </div>
      )
    }

    if (!fileData) return null

    if (fileData.is_binary) {
      if (fileData.mime_type?.startsWith('image/')) {
        return (
          <div className="file-preview-image-wrap">
            <img
              src={`data:${fileData.mime_type};base64,${fileData.content}`}
              alt={fileData.name}
              className="file-preview-image"
            />
          </div>
        )
      }
      return (
        <div className="file-preview-status binary">
          <IconFile size={32} strokeWidth={1.8} />
          <span>二进制文件 ({fileData.mime_type || '未知类型'})</span>
          <span className="file-status-sub">不支持在线文本预览</span>
        </div>
      )
    }

    if (isMarkdown && viewMode === 'render') {
      return (
        <div className="file-preview-markdown">
          <Markdown text={fileData.content} />
        </div>
      )
    }

    return (
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
    )
  }

  return (
    <aside
      className={`file-sidebar${isDualPane ? ' dual-pane' : ''}`}
      style={{ width: `${width}px` }}
      aria-label="工程文件浏览器"
    >
      {/* 左侧：文件代码预览区（处于双栏模式或单独预览模式时展示） */}
      {(isDualPane || (activePath && !isDualPane)) && (
        <div className="file-sidebar-preview-pane">
          <div className="file-preview-head">
            <div className="file-preview-title-wrap" title={activePath || ''}>
              {!isDualPane && (
                <button
                  type="button"
                  className="icon-ghost file-nav-back"
                  onClick={() => onSelectPath(null)}
                  title="返回文件列表"
                  aria-label="返回文件列表"
                >
                  <IconArrowLeft size={16} />
                </button>
              )}
              <div className="file-preview-title-text">
                <span className="file-nav-name">
                  {activeFileInfo?.name || activePath?.split('/').pop() || activePath}
                </span>
                <span className="file-nav-path">{activePath}</span>
              </div>
            </div>

            <div className="file-preview-actions">
              {fileData && (
                <div className="file-meta-tags">
                  {!fileData.is_binary && (
                    <span className="file-meta-item">{lines.length} 行</span>
                  )}
                  <span className="file-meta-item">{fmtBytes(fileData.size)}</span>
                </div>
              )}

              {isMarkdown && !loading && !error && fileData && !fileData.is_binary && (
                <div className="file-view-toggle">
                  <button
                    type="button"
                    className={`file-toggle-btn${viewMode === 'render' ? ' active' : ''}`}
                    onClick={() => setViewMode('render')}
                  >
                    预览
                  </button>
                  <button
                    type="button"
                    className={`file-toggle-btn${viewMode === 'source' ? ' active' : ''}`}
                    onClick={() => setViewMode('source')}
                  >
                    源码
                  </button>
                </div>
              )}

              {fileData && !fileData.is_binary && <CopyButton text={fileData.content} />}

              {activePath && (
                <a
                  className="icon-ghost file-download"
                  href={
                    sessionId
                      ? api.sessionFileDownloadUrl(sessionId, activePath)
                      : api.fsFileDownloadUrl(activePath, workspace || undefined)
                  }
                  download
                  title="下载文件"
                  aria-label="下载文件"
                >
                  <IconDownload size={16} />
                </a>
              )}

              <button
                type="button"
                className="icon-ghost file-preview-close"
                onClick={() => onSelectPath(null)}
                title="关闭文件预览"
                aria-label="关闭文件预览"
              >
                <IconX size={16} />
              </button>
            </div>
          </div>

          <div className="file-preview-body" ref={scrollerRef}>
            {renderPreviewBody()}
          </div>
        </div>
      )}

      {/* 右侧：工程目录树导航区（双栏时靠右锚定，单栏时占满） */}
      {(isDualPane || !activePath) && (
        <div className="file-sidebar-nav-pane">
          <div className="file-nav-head">
            <div className="file-sidebar-tabs">
              <button
                type="button"
                className={`file-tab-btn${activeTab === 'tree' ? ' active' : ''}`}
                onClick={() => setActiveTab('tree')}
              >
                工程文件
              </button>
              <button
                type="button"
                className={`file-tab-btn${activeTab === 'related' ? ' active' : ''}`}
                onClick={() => setActiveTab('related')}
              >
                会话变动
                {files.length > 0 && (
                  <span className="file-count-badge">{files.length}</span>
                )}
              </button>
            </div>

            <button
              type="button"
              className="icon-ghost file-sidebar-close"
              onClick={onClose}
              title="关闭侧边栏"
              aria-label="关闭侧边栏"
            >
              <IconX size={16} />
            </button>
          </div>

          <div className="file-sidebar-list-container">
            {renderNavList()}
          </div>
        </div>
      )}
    </aside>
  )
}
