import { useEffect, useMemo, useRef, useState, type KeyboardEvent, type ReactNode } from 'react'
import { api, toError } from '../../api'
import { useFileTree } from '../../hooks/useFileTree'
import type { FileFocus } from '../../hooks/usePanelLayout'
import type { FsEntry, FsFindEntry, RelatedFile } from '../../types'
import { parentDir } from '../../utils/highlight'
import { IconChevronRight, IconSearch } from '../icons'
import { cleanPath, relativePath } from '../trace/describe'

/** 会话文件默认展示条数，其余折叠 */
const SESSION_PREVIEW = 8

export function isChanged(file: RelatedFile): boolean {
  return file.actions.includes('edit') || file.actions.includes('write')
}

/** 打开会话文件时的定位：有改动定位改动处，否则定位最近一次读取区间 */
export function sessionFileFocus(file: RelatedFile): FileFocus {
  if (isChanged(file)) return 'changes'
  return file.readRanges.at(-1) ?? null
}

export function isMac(): boolean {
  return /Mac|iPhone|iPad/.test(navigator.platform)
}

/** 行内上下方向键在同一容器的可聚焦行之间移动 */
function moveRowFocus(event: KeyboardEvent<HTMLElement>, selector: string) {
  if (event.key !== 'ArrowDown' && event.key !== 'ArrowUp') return false
  const container = event.currentTarget.closest('.explorer')
  if (!container) return false
  const rows = [...container.querySelectorAll<HTMLElement>(selector)]
  const index = rows.indexOf(event.target as HTMLElement)
  const next = rows[index + (event.key === 'ArrowDown' ? 1 : -1)]
  if (next) {
    event.preventDefault()
    next.focus()
  }
  return true
}

function Highlighted({ text, indices, offset }: { text: string; indices: Set<number>; offset: number }) {
  const chars = [...text]
  return (
    <>
      {chars.map((char, index) =>
        indices.has(index + offset) ? (
          <mark key={index}>{char}</mark>
        ) : (
          <span key={index}>{char}</span>
        ),
      )}
    </>
  )
}

function SearchResults({
  results,
  selected,
  onOpen,
  onHover,
}: {
  results: FsFindEntry[]
  selected: number
  onOpen: (entry: FsFindEntry) => void
  onHover: (index: number) => void
}) {
  return (
    <div className="explorer-results" role="listbox" aria-label="匹配文件">
      {results.map((entry, index) => {
        const dir = parentDir(entry.path)
        const hits = new Set(entry.indices)
        const dirLength = dir ? [...dir].length + 1 : 0
        return (
          <button
            key={entry.path}
            type="button"
            role="option"
            aria-selected={index === selected}
            className={`x-row${index === selected ? ' selected' : ''}`}
            onClick={() => onOpen(entry)}
            onMouseMove={() => onHover(index)}
          >
            <span className="x-name">
              <Highlighted text={entry.name} indices={hits} offset={dirLength} />
            </span>
            {dir && (
              <span className="x-dir">
                <Highlighted text={dir} indices={hits} offset={0} />
              </span>
            )}
          </button>
        )
      })}
    </div>
  )
}

function SessionFiles({
  files,
  activeRel,
  onOpen,
}: {
  files: RelatedFile[]
  activeRel: string | null
  onOpen: (file: RelatedFile) => void
}) {
  const [expanded, setExpanded] = useState(false)
  const shown = expanded ? files : files.slice(0, SESSION_PREVIEW)
  return (
    <section className="explorer-section">
      <h3 className="explorer-label">
        本次会话 <span className="explorer-count">{files.length}</span>
      </h3>
      {shown.map(file => (
        <button
          key={file.rel}
          type="button"
          className={`x-row${file.rel === activeRel ? ' active' : ''}${file.deleted ? ' deleted' : ''}`}
          onClick={() => onOpen(file)}
          onKeyDown={event => moveRowFocus(event, '.x-row, .tree-row')}
          title={file.rel}
        >
          <span className={`glyph ${isChanged(file) ? 'change' : 'observe'}`} aria-hidden />
          <span className="x-name">{file.name}</span>
          <span className="x-dir">{file.dir}</span>
          <span className="x-meta">
            {file.deleted ? (
              <span className="diff-del">已删除</span>
            ) : isChanged(file) ? (
              <>
                <span className="diff-add">+{file.plus}</span>
                <span className="diff-del">−{file.minus}</span>
              </>
            ) : file.actions.includes('attachment') ? (
              '附件'
            ) : (
              '读'
            )}
          </span>
        </button>
      ))}
      {files.length > SESSION_PREVIEW && (
        <button type="button" className="explorer-more" onClick={() => setExpanded(value => !value)}>
          {expanded ? '收起' : `显示全部 ${files.length} 个`}
        </button>
      )}
    </section>
  )
}

interface TreeContext {
  expandedDirs: Set<string>
  dirChildren: Record<string, FsEntry[]>
  dirLoading: Record<string, boolean>
  activeRel: string | null
  marks: Map<string, RelatedFile>
  changedDirs: Set<string>
  toggleDir: (path: string) => void
  expandDir: (path: string) => void
  onOpenFile: (entry: FsEntry) => void
}

function TreeRows({ entries, depth, ctx }: { entries: FsEntry[]; depth: number; ctx: TreeContext }) {
  return (
    <>
      {entries.map(entry => {
        const open = entry.is_dir && ctx.expandedDirs.has(entry.path)
        const mark = ctx.marks.get(entry.path)
        const onKeyDown = (event: KeyboardEvent<HTMLButtonElement>) => {
          if (moveRowFocus(event, '.x-row, .tree-row')) return
          if (!entry.is_dir) return
          if (event.key === 'ArrowRight' && !open) {
            event.preventDefault()
            ctx.expandDir(entry.path)
          } else if (event.key === 'ArrowLeft' && open) {
            event.preventDefault()
            ctx.toggleDir(entry.path)
          }
        }
        return (
          <div key={entry.path} role="none">
            <button
              type="button"
              role="treeitem"
              aria-level={depth + 1}
              aria-expanded={entry.is_dir ? open : undefined}
              aria-selected={entry.path === ctx.activeRel}
              className={`tree-row ${entry.is_dir ? 'dir' : 'file'}${entry.path === ctx.activeRel ? ' active' : ''}`}
              style={{ '--depth': depth } as React.CSSProperties}
              onClick={() => (entry.is_dir ? ctx.toggleDir(entry.path) : ctx.onOpenFile(entry))}
              onKeyDown={onKeyDown}
              title={entry.path}
              data-path={entry.path}
            >
              <span className={`tree-twist${open ? ' open' : ''}`} aria-hidden>
                {entry.is_dir && <IconChevronRight size={12} strokeWidth={2} />}
              </span>
              <span className="tree-name">{entry.name}</span>
              {ctx.dirLoading[entry.path] ? (
                <span className="tree-mark loading" aria-hidden />
              ) : mark ? (
                <span className={`glyph ${isChanged(mark) ? 'change' : 'observe'}`} aria-label={isChanged(mark) ? '本会话修改' : '本会话读取'} />
              ) : entry.is_dir && ctx.changedDirs.has(entry.path) ? (
                <span className="tree-mark dot" aria-label="含本会话修改" />
              ) : null}
            </button>
            {open && (
              <div role="group">
                {(ctx.dirChildren[entry.path] ?? []).length === 0 && !ctx.dirLoading[entry.path] ? (
                  <div className="tree-empty" style={{ '--depth': depth + 1 } as React.CSSProperties}>
                    空目录
                  </div>
                ) : (
                  <TreeRows entries={ctx.dirChildren[entry.path] ?? []} depth={depth + 1} ctx={ctx} />
                )}
              </div>
            )}
          </div>
        )
      })}
    </>
  )
}

/**
 * 文件浏览：跳转搜索（后端模糊匹配）→ 本次会话涉及的文件 → 工程目录树。
 * 标记沿用工作轨迹的字形语言：● 本会话修改、○ 仅读取。
 */
export function Explorer({
  workspace,
  files,
  activePath,
  enabled,
  searchSeq,
  revealDir,
  head,
  onOpen,
}: {
  workspace: string
  files: RelatedFile[]
  activePath: string | null
  /** 面板可见时才加载目录 */
  enabled: boolean
  /** 递增即聚焦搜索框 */
  searchSeq: number
  /** 面包屑点击的目录：展开并滚入视区 */
  revealDir: { path: string; seq: number } | null
  head?: ReactNode
  onOpen: (path: string, focus?: FileFocus) => void
}) {
  const tree = useFileTree(workspace, enabled)
  const { rootEntries, rootLoading, rootError, dirChildren, dirLoading, expandedDirs } = tree
  const [query, setQuery] = useState('')
  const [results, setResults] = useState<FsFindEntry[] | null>(null)
  const [searchError, setSearchError] = useState<string | null>(null)
  const [selected, setSelected] = useState(0)
  const inputRef = useRef<HTMLInputElement>(null)
  const bodyRef = useRef<HTMLDivElement>(null)

  const activeRel = activePath ? relativePath(cleanPath(activePath), workspace) : null
  const marks = useMemo(() => new Map(files.map(file => [file.rel, file])), [files])
  const changedDirs = useMemo(() => {
    const dirs = new Set<string>()
    for (const file of files) {
      if (!isChanged(file)) continue
      let dir = parentDir(file.rel)
      while (dir) {
        dirs.add(dir)
        dir = parentDir(dir)
      }
    }
    return dirs
  }, [files])

  useEffect(() => {
    if (searchSeq > 0) {
      inputRef.current?.focus()
      inputRef.current?.select()
    }
  }, [searchSeq])

  // 输入去抖后查询；结果与查询错位时丢弃
  useEffect(() => {
    const q = query.trim()
    if (!q) {
      setResults(null)
      setSearchError(null)
      return
    }
    let cancelled = false
    const timer = setTimeout(() => {
      api
        .fsFind(q, workspace, 40)
        .then(response => {
          if (cancelled) return
          setResults(response.entries)
          setSearchError(null)
          setSelected(0)
        })
        .catch(error => {
          if (!cancelled) setSearchError(toError(error))
        })
    }, 110)
    return () => {
      cancelled = true
      clearTimeout(timer)
    }
  }, [query, workspace])

  // 当前文件：在树中展开其上级目录并滚入视区
  const { revealPath, expandDir } = tree
  useEffect(() => {
    if (activeRel && !activeRel.startsWith('/')) revealPath(activeRel)
  }, [activeRel, revealPath])

  useEffect(() => {
    if (!activeRel) return
    const row = bodyRef.current?.querySelector<HTMLElement>(`.tree-row[data-path="${CSS.escape(activeRel)}"]`)
    row?.scrollIntoView({ block: 'nearest' })
  }, [activeRel, dirChildren])

  useEffect(() => {
    if (!revealDir) return
    revealPath(`${revealDir.path}/_`)
    expandDir(revealDir.path)
    requestAnimationFrame(() => {
      const row = bodyRef.current?.querySelector<HTMLElement>(`.tree-row[data-path="${CSS.escape(revealDir.path)}"]`)
      row?.scrollIntoView({ block: 'start' })
      row?.focus()
    })
  }, [revealDir, revealPath, expandDir])

  const openResult = (entry: FsFindEntry) => {
    setQuery('')
    onOpen(entry.path)
  }

  const onSearchKey = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key === 'Escape') {
      if (query) {
        event.stopPropagation()
        setQuery('')
      }
      return
    }
    if (!results || results.length === 0) {
      if (event.key === 'ArrowDown') {
        event.preventDefault()
        bodyRef.current?.querySelector<HTMLElement>('.x-row, .tree-row')?.focus()
      }
      return
    }
    if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      event.preventDefault()
      const delta = event.key === 'ArrowDown' ? 1 : -1
      setSelected(index => (index + delta + results.length) % results.length)
    } else if (event.key === 'Enter') {
      event.preventDefault()
      const entry = results[selected]
      if (entry) openResult(entry)
    }
  }

  const ctx: TreeContext = {
    expandedDirs,
    dirChildren,
    dirLoading,
    activeRel,
    marks,
    changedDirs,
    toggleDir: tree.toggleDir,
    expandDir,
    onOpenFile: entry => {
      const mark = marks.get(entry.path)
      onOpen(mark?.path ?? entry.path, mark ? sessionFileFocus(mark) : null)
    },
  }

  const projectName = workspace.split('/').filter(Boolean).pop() ?? workspace

  return (
    <div className="explorer">
      {head}
      <div className="explorer-search">
        <IconSearch size={14} strokeWidth={2} />
        <input
          ref={inputRef}
          type="search"
          value={query}
          placeholder="跳转到文件"
          aria-label="按名称跳转到文件"
          spellCheck={false}
          onChange={event => setQuery(event.target.value)}
          onKeyDown={onSearchKey}
        />
        {!query && <kbd>{isMac() ? '⌘P' : 'Ctrl P'}</kbd>}
      </div>
      <div className="explorer-body" ref={bodyRef}>
        {query.trim() ? (
          searchError ? (
            <p className="explorer-note error">搜索失败：{searchError}</p>
          ) : results == null ? (
            <p className="explorer-note">搜索中…</p>
          ) : results.length === 0 ? (
            <p className="explorer-note">没有名称匹配「{query.trim()}」的文件</p>
          ) : (
            <SearchResults results={results} selected={selected} onOpen={openResult} onHover={setSelected} />
          )
        ) : (
          <>
            {files.length > 0 && (
              <SessionFiles files={files} activeRel={activeRel} onOpen={file => onOpen(file.path, sessionFileFocus(file))} />
            )}
            <section className="explorer-section">
              <h3 className="explorer-label" title={workspace}>
                工程目录
              </h3>
              {rootLoading ? (
                <p className="explorer-note">正在读取目录…</p>
              ) : rootError ? (
                <p className="explorer-note error">{rootError}</p>
              ) : rootEntries.length === 0 ? (
                <p className="explorer-note">工程目录为空</p>
              ) : (
                <div className="tree" role="tree" aria-label={`${projectName} 目录`}>
                  <TreeRows entries={rootEntries} depth={0} ctx={ctx} />
                </div>
              )}
            </section>
          </>
        )}
      </div>
    </div>
  )
}
