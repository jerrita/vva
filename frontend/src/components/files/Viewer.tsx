import { useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import hljs from 'highlight.js/lib/common'
import { api, toError } from '../../api'
import type { FileFocus } from '../../hooks/usePanelLayout'
import { usePersistentFlag } from '../../hooks/usePersistentState'
import type { RelatedFile, SessionFileResponse } from '../../types'
import { changedLines } from '../../utils/changeMarks'
import type { FileDiff } from '../../utils/editDiff'
import { fmtBytes, languageForPath, splitHighlightedLines } from '../../utils/highlight'
import { CopyButton } from '../CopyButton'
import { IconChevronDown, IconChevronUp, IconDownload, IconWrap, IconX } from '../icons'
import { useLightbox } from '../Lightbox'
import { Markdown } from '../Markdown'
import { cleanPath, relativePath } from '../trace/describe'

/** 连续改动行合并为区块起点，供上一处/下一处跳转 */
function changeStarts(marks: Set<number>): number[] {
  return [...marks].sort((a, b) => a - b).filter(line => !marks.has(line - 1))
}

function AudioPreview({ src, name }: { src: string; name: string }) {
  const [failed, setFailed] = useState(false)
  useEffect(() => setFailed(false), [src])

  return (
    <div className="viewer-audio">
      <audio controls preload="metadata" src={src} aria-label={`播放 ${name}`} onError={() => setFailed(true)} />
      {failed && (
        <p className="viewer-note error" role="alert">音频加载失败或浏览器不支持此格式，可下载查看。</p>
      )}
    </div>
  )
}

function Breadcrumb({
  rel,
  onReveal,
}: {
  rel: string
  onReveal?: (dir: string) => void
}) {
  const absolute = rel.startsWith('/')
  const segments = rel.split('/').filter(Boolean)
  const name = segments.pop() ?? rel
  return (
    <nav className="crumbs" aria-label="文件路径">
      {segments.map((segment, index) => {
        const dir = segments.slice(0, index + 1).join('/')
        return (
          <span key={dir} className="crumb-seg">
            {onReveal && !absolute ? (
              <button type="button" className="crumb" onClick={() => onReveal(dir)} title={`在文件树中定位 ${dir}`}>
                {segment}
              </button>
            ) : (
              <span className="crumb">{segment}</span>
            )}
            <span className="crumb-sep" aria-hidden>
              /
            </span>
          </span>
        )
      })}
      <span className="crumb-file">{name}</span>
    </nav>
  )
}

/**
 * 文件查看：面包屑 + 工具行 + 代码（行号常驻、本会话改动行紫色竖条、读取区间底色）。
 * focus 为读取区间时高亮并滚入视区；为 'changes' 时定位首处改动。
 */
export function Viewer({
  sessionId,
  workspace,
  path,
  focus,
  focusSeq,
  related,
  diffs,
  leading,
  trailing,
  onReveal,
}: {
  sessionId: string | null
  workspace: string
  path: string
  focus: FileFocus
  focusSeq: number
  related?: RelatedFile
  diffs: FileDiff[]
  /** 头部前置（返回按钮） */
  leading?: ReactNode
  /** 头部尾置（关闭按钮） */
  trailing?: ReactNode
  onReveal?: (dir: string) => void
}) {
  const openLightbox = useLightbox()
  const [file, setFile] = useState<SessionFileResponse | null>(null)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [rendered, setRendered] = useState(true)
  const [wrap, setWrap] = usePersistentFlag('vva.viewer-wrap')
  const [range, setRange] = useState<{ start: number; end: number } | null>(null)
  const bodyRef = useRef<HTMLDivElement>(null)
  const rel = relativePath(cleanPath(path), workspace)

  useEffect(() => {
    let cancelled = false
    setLoading(true)
    setError(null)
    const request = sessionId ? api.sessionFile(sessionId, path) : api.fsFile(path, workspace)
    request
      .then(response => {
        if (!cancelled) setFile(response)
      })
      .catch(err => {
        if (!cancelled) {
          setFile(null)
          setError(toError(err))
        }
      })
      .finally(() => {
        if (!cancelled) setLoading(false)
      })
    return () => {
      cancelled = true
    }
  }, [sessionId, workspace, path])

  const text = file && !file.is_binary ? file.content : null
  const lines = useMemo(() => (text == null ? [] : text.replace(/\n$/, '').split('\n')), [text])
  const highlighted = useMemo(() => {
    const language = languageForPath(rel)
    if (text == null || !language || !hljs.getLanguage(language) || text.length > 400_000) return null
    return splitHighlightedLines(hljs.highlight(text.replace(/\n$/, ''), { language }).value)
  }, [text, rel])
  const marks = useMemo(() => (text == null ? new Set<number>() : changedLines(text, diffs)), [text, diffs])
  const starts = useMemo(() => changeStarts(marks), [marks])
  const isMarkdown = /\.(md|markdown)$/i.test(rel)
  const showRendered = isMarkdown && rendered

  const scrollToLine = (line: number, block: ScrollLogicalPosition = 'center') => {
    const row = bodyRef.current?.querySelector<HTMLElement>(`[data-line="${line}"]`)
    row?.scrollIntoView({ block })
  }

  // 定位请求：内容就绪后执行（同一文件重复请求按 focusSeq 重新定位）
  useEffect(() => {
    if (text == null) return
    if (focus === 'changes') {
      setRange(null)
      if (starts.length > 0) {
        setRendered(false)
        requestAnimationFrame(() => scrollToLine(starts[0]))
      } else {
        bodyRef.current?.scrollTo({ top: 0 })
      }
    } else if (focus) {
      setRange(focus)
      setRendered(false)
      requestAnimationFrame(() => scrollToLine(focus.start, 'start'))
    } else {
      setRange(null)
      bodyRef.current?.scrollTo({ top: 0 })
    }
    // starts 随 text 同步变化，定位只随请求触发
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [text, focusSeq])

  const jumpChange = (direction: 1 | -1) => {
    const body = bodyRef.current
    if (!body || starts.length === 0) return
    const rows = starts.map(line => body.querySelector<HTMLElement>(`[data-line="${line}"]`))
    const mid = body.getBoundingClientRect().top + body.clientHeight / 2
    const positions = rows.map(row => (row ? row.getBoundingClientRect().top : 0))
    let target: number
    if (direction === 1) {
      const next = positions.findIndex(top => top > mid + 4)
      target = next === -1 ? 0 : next
    } else {
      let previous = -1
      positions.forEach((top, index) => {
        if (top < mid - 4) previous = index
      })
      target = previous === -1 ? starts.length - 1 : previous
    }
    scrollToLine(starts[target])
  }

  const downloadUrl = sessionId ? api.sessionFileDownloadUrl(sessionId, path) : api.fsFileDownloadUrl(path, workspace)
  const previewPath = file?.path ?? path
  const previewUrl = sessionId ? api.sessionFilePreviewUrl(sessionId, previewPath) : api.fsFilePreviewUrl(previewPath, workspace)

  let body: ReactNode
  if (loading && !file) {
    body = <p className="viewer-note">正在读取…</p>
  } else if (error) {
    body = <p className="viewer-note error">无法读取文件：{error}</p>
  } else if (!file) {
    body = null
  } else if (file.is_binary) {
    if (file.mime_type?.startsWith('image/')) {
      const src = `data:${file.mime_type};base64,${file.content}`
      body = (
        <div className="viewer-image">
          <img src={src} alt={file.name} className="lightboxable" onClick={() => openLightbox(src)} />
        </div>
      )
    } else if (file.mime_type?.startsWith('audio/')) {
      body = <AudioPreview src={previewUrl} name={file.name} />
    } else if (file.mime_type === 'application/pdf') {
      body = (
        <object className="viewer-pdf" data={`${previewUrl}#view=FitH&navpanes=0`} type="application/pdf" aria-label={`预览 ${file.name}`}>
          <p className="viewer-note">浏览器无法显示 PDF，<a href={downloadUrl} download>下载查看</a>。</p>
        </object>
      )
    } else {
      body = <p className="viewer-note">二进制文件（{file.mime_type || '未知类型'}），不提供文本预览，可下载查看。</p>
    }
  } else if (showRendered) {
    body = (
      <div className="viewer-markdown">
        <Markdown text={file.content} />
      </div>
    )
  } else {
    body = (
      <div className={`code${wrap ? ' wrap' : ''}`} style={{ '--ln-width': `${String(lines.length).length + 1}ch` } as React.CSSProperties}>
        {lines.map((line, index) => {
          const number = index + 1
          const inRange = range != null && number >= range.start && number <= range.end
          return (
            <div
              key={index}
              data-line={number}
              className={`code-row${marks.has(number) ? ' changed' : ''}${inRange ? ' in-range' : ''}`}
            >
              <span className="ln">{number}</span>
              {highlighted ? (
                <span className="code-text" dangerouslySetInnerHTML={{ __html: highlighted[index] || ' ' }} />
              ) : (
                <span className="code-text">{line || ' '}</span>
              )}
            </div>
          )
        })}
      </div>
    )
  }

  return (
    <div className="viewer">
      <div className="viewer-head">
        {leading}
        <Breadcrumb rel={rel} onReveal={onReveal} />
        <div className="viewer-actions">
          {isMarkdown && text != null && (
            <div className="viewer-switch" role="group" aria-label="Markdown 显示方式">
              <button type="button" aria-pressed={rendered} onClick={() => setRendered(true)}>
                预览
              </button>
              <button type="button" aria-pressed={!rendered} onClick={() => setRendered(false)}>
                源码
              </button>
            </div>
          )}
          {text != null && !showRendered && (
            <button
              type="button"
              className={`icon-ghost${wrap ? ' active' : ''}`}
              aria-pressed={wrap}
              onClick={() => setWrap(value => !value)}
              title={wrap ? '取消自动换行' : '自动换行'}
              aria-label="自动换行"
            >
              <IconWrap size={15} strokeWidth={1.8} />
            </button>
          )}
          {text != null && <CopyButton text={text} />}
          <a className="icon-ghost" href={downloadUrl} download title="下载" aria-label="下载">
            <IconDownload size={15} strokeWidth={1.8} />
          </a>
          {trailing}
        </div>
      </div>
      {file && (
        <div className="viewer-meta">
          {text != null && <span>{lines.length} 行</span>}
          <span>{fmtBytes(file.size)}</span>
          {related && (related.plus > 0 || related.minus > 0) && (
            <span className="viewer-changes">
              <span className="glyph change" aria-hidden />
              本会话
              <span className="diff-add">+{related.plus}</span>
              <span className="diff-del">−{related.minus}</span>
              {starts.length > 0 && !showRendered && (
                <span className="viewer-jump">
                  <span>{starts.length} 处</span>
                  <button type="button" onClick={() => jumpChange(-1)} title="上一处改动" aria-label="上一处改动">
                    <IconChevronUp size={13} />
                  </button>
                  <button type="button" onClick={() => jumpChange(1)} title="下一处改动" aria-label="下一处改动">
                    <IconChevronDown size={13} />
                  </button>
                </span>
              )}
            </span>
          )}
          {range && !showRendered && (
            <span className="viewer-range">
              L{range.start}–{range.end}
              <button type="button" onClick={() => setRange(null)} title="取消区间高亮" aria-label="取消区间高亮">
                <IconX size={11} />
              </button>
            </span>
          )}
        </div>
      )}
      <div className="viewer-body" ref={bodyRef} tabIndex={-1}>
        {body}
      </div>
    </div>
  )
}
