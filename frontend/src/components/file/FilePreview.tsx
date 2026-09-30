import { useEffect, useMemo, useRef, useState } from 'react'
import hljs from 'highlight.js/lib/common'
import { api, toError } from '../../api'
import type { RelatedFile, SessionFileResponse } from '../../types'
import { CopyButton } from '../CopyButton'
import { Markdown } from '../Markdown'
import { useLightbox } from '../Lightbox'
import {
  IconArrowLeft,
  IconDownload,
  IconFile,
  IconX,
} from '../icons'
import { fmtBytes, languageForPath, splitHighlightedLines } from '../../utils/highlight'

interface FilePreviewProps {
  sessionId: string | null
  workspace?: string | null
  activePath: string | null
  /** 会话相关文件（用于标题回退显示） */
  files: RelatedFile[]
  /** 双栏模式：为 false 时展示返回按钮 */
  isDualPane: boolean
  onClose: () => void
}

/** 文件预览面板：会话/工程文件内容加载、代码高亮、Markdown 渲染与下载。 */
export function FilePreview({
  sessionId,
  workspace,
  activePath,
  files,
  isDualPane,
  onClose,
}: FilePreviewProps) {
  const openLightbox = useLightbox()
  const [fileData, setFileData] = useState<SessionFileResponse | null>(null)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [viewMode, setViewMode] = useState<'render' | 'source'>('render')

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

  const renderBody = () => {
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
        const src = `data:${fileData.mime_type};base64,${fileData.content}`
        return (
          <div className="file-preview-image-wrap">
            <img
              src={src}
              alt={fileData.name}
              className="file-preview-image lightboxable"
              onClick={() => openLightbox(src)}
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
    <div className="file-sidebar-preview-pane">
      <div className="file-preview-head">
        <div className="file-preview-title-wrap" title={activePath || ''}>
          {!isDualPane && (
            <button
              type="button"
              className="icon-ghost file-nav-back"
              onClick={onClose}
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
              {!fileData.is_binary && <span className="file-meta-item">{lines.length} 行</span>}
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
            onClick={onClose}
            title="关闭文件预览"
            aria-label="关闭文件预览"
          >
            <IconX size={16} />
          </button>
        </div>
      </div>

      <div className="file-preview-body" ref={scrollerRef}>
        {renderBody()}
      </div>
    </div>
  )
}
