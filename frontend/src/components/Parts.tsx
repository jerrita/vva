import { useState } from 'react'
import type { Annotation, ContentPart, ToolCall, ToolResult, WebSearchCall } from '../types'
import { Markdown } from './Markdown'

/** 与 TUI 对齐的结果预览行数上限 */
const PREVIEW_LIMIT = 10

function truncate(text: string, max: number): string {
  return text.length > max ? `${text.slice(0, max)}…` : text
}

function parseArgs(args: string): Record<string, unknown> | null {
  try {
    const value: unknown = JSON.parse(args)
    return value && typeof value === 'object' ? (value as Record<string, unknown>) : null
  } catch {
    return null
  }
}

function str(value: unknown): string | null {
  return typeof value === 'string' ? value : null
}

function lineCount(text: string): number {
  return text === '' ? 0 : text.split('\n').length
}

function editEntries(args: Record<string, unknown>): Record<string, unknown>[] {
  if (args.edits === undefined) return [args]
  if (!Array.isArray(args.edits)) return []
  return args.edits.filter((edit): edit is Record<string, unknown> =>
    edit !== null && typeof edit === 'object' && !Array.isArray(edit))
}

/** 摘要行的按工具类型 detail，对齐 TUI tool_call_detail */
interface ToolDetail {
  text: string
  filePath?: string
  /** Edit 专用：增删行数着色展示 */
  editPath?: string
  plus?: number
  minus?: number
}

function toolDetail(call: ToolCall, result: ToolResult | null): ToolDetail {
  const args = parseArgs(call.arguments)
  if (!args) return { text: truncate(call.arguments, 88) }
  switch (call.name) {
    case 'Read': {
      const rawPath = str(args.path)
      const path = rawPath ?? truncate(call.arguments, 88)
      const offset = typeof args.offset === 'number' ? args.offset : 1
      const limit = typeof args.limit === 'number' ? String(args.limit) : 'all'
      const byteOffset = typeof args.byte_offset === 'number' ? `, byte_offset=${args.byte_offset}` : ''
      return {
        text: `${path} [offset=${offset}, limit=${limit}${byteOffset}]`,
        filePath: rawPath ?? undefined,
      }
    }
    case 'Write': {
      const rawPath = str(args.path)
      return { text: rawPath ?? truncate(call.arguments, 88), filePath: rawPath ?? undefined }
    }
    case 'Edit': {
      const path = str(args.path)
      const edits = editEntries(args)
      if (path && edits.length > 0 && edits.every(edit => str(edit.old_string) !== null && str(edit.new_string) !== null)) {
        return {
          text: path,
          filePath: path,
          editPath: path,
          plus: edits.reduce((count, edit) => count + lineCount(str(edit.new_string)!), 0),
          minus: edits.reduce((count, edit) => count + lineCount(str(edit.old_string)!), 0),
        }
      }
      return { text: truncate(call.arguments, 88), filePath: path ?? undefined }
    }
    case 'Glob': {
      const pattern = str(args.pattern) ?? truncate(call.arguments, 88)
      if (result && !result.is_error) {
        const count = result.content
          .split('\n')
          .filter(line => line !== '' && line !== '[results truncated]').length
        return { text: `"${pattern}" (${count} ${count === 1 ? 'match' : 'matches'})` }
      }
      return { text: `"${pattern}"` }
    }
    case 'Grep': {
      const pattern = str(args.pattern)
      if (!pattern) return { text: truncate(call.arguments, 88) }
      const include = str(args.include)
      return { text: include ? `${include}, ${pattern}` : pattern }
    }
    case 'Shell':
      return { text: str(args.intent) ?? truncate(call.arguments, 88) }
    default:
      return { text: truncate(call.arguments, 88) }
  }
}

/** 主体首行的命令/路径预览，对齐 TUI tool_block_command */
function toolCommand(call: ToolCall): string | null {
  const args = parseArgs(call.arguments)
  if (!args) return null
  switch (call.name) {
    case 'Shell': {
      const command = str(args.command)
      return command ? `$ ${command}` : null
    }
    case 'Glob': {
      const pattern = str(args.pattern)
      return pattern ? `"${pattern}"` : null
    }
    case 'Grep':
      return str(args.pattern)
    default:
      return null
  }
}

/** 工具调用与其结果配对后的节点 */
interface ToolGroupNode {
  call: ToolCall
  result: ToolResult | null
}

function ToolGroupView({
  node,
  expanded = false,
  onPreviewFile,
}: {
  node: ToolGroupNode
  expanded?: boolean
  onPreviewFile?: (path: string) => void
}) {
  const [showAll, setShowAll] = useState(false)
  const { call, result } = node
  const detail = toolDetail(call, result)
  const isError = result?.is_error ?? false
  const isFileTool = call.name === 'Read' || call.name === 'Write' || call.name === 'Edit'

  if (isFileTool) {
    return (
      <div
        className={`file-tool-row${isError ? ' error' : ''}${
          detail.filePath && onPreviewFile ? ' clickable' : ''
        }`}
        onClick={
          detail.filePath && onPreviewFile
            ? () => onPreviewFile(detail.filePath!)
            : undefined
        }
        title={
          detail.filePath
            ? `在右侧边栏预览 ${detail.filePath}`
            : undefined
        }
      >
        {result ? (
          <span className={`file-tool-icon status ${isError ? 'error' : 'ok'}`}>
            {isError ? '✕' : '✓'}
          </span>
        ) : (
          <span className="file-tool-icon status pending" />
        )}
        <code className="file-tool-name">{call.name}</code>
        {detail.editPath ? (
          <span className="file-tool-intent">
            <span className="file-name">{detail.editPath}</span>{' '}
            <span className="diff-count ins">(+{detail.plus})</span>
            {', '}
            <span className="diff-count del">(-{detail.minus})</span>
          </span>
        ) : detail.filePath ? (
          <span className="file-tool-intent">
            <span className="file-name">{detail.filePath}</span>
            {detail.text.slice(detail.filePath.length)}
          </span>
        ) : (
          <span className="file-tool-intent">{detail.text}</span>
        )}
        <div className="file-tool-right">
          {isError && result && (
            <span className="file-tool-error-snippet" title={result.content}>
              {result.content.split('\n')[0]}
            </span>
          )}
          {detail.filePath && onPreviewFile && (
            <span className="file-tool-preview-hint" title="预览文件" aria-hidden>
              <svg
                viewBox="0 0 24 24"
                width="13"
                height="13"
                fill="none"
                stroke="currentColor"
                strokeWidth="1.8"
                strokeLinecap="round"
                strokeLinejoin="round"
              >
                <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
                <polyline points="14 2 14 8 20 8" />
              </svg>
            </span>
          )}
        </div>
      </div>
    )
  }

  const command = toolCommand(call)
  const resultLines = result ? result.content.split('\n') : []
  const preview = resultLines.slice(0, PREVIEW_LIMIT)
  const rest = resultLines.length - preview.length

  return (
    <details className="block tool" open={expanded}>
      <summary>
        {result ? (
          <span className={`block-icon status ${isError ? 'error' : 'ok'}`}>
            {isError ? '✕' : '✓'}
          </span>
        ) : (
          <span className="block-icon status pending" />
        )}
        <code>{call.name}</code>
        <span className="tool-intent">{detail.text}</span>
      </summary>
      {command && <pre className="tool-cmd">{command}</pre>}
      {result && !isError && !showAll && (
        <pre>{preview.join('\n') || ' '}</pre>
      )}
      {result && isError && <pre>{result.content}</pre>}
      {result && !isError && rest > 0 && (
        <details
          className="tool-more"
          open={showAll}
          onToggle={e => setShowAll(e.currentTarget.open)}
        >
          <summary>展开全部（共 {resultLines.length} 行）</summary>
          <pre>{resultLines.join('\n')}</pre>
        </details>
      )}
    </details>
  )
}

function WebSearchView({ call, expanded }: { call: WebSearchCall; expanded: boolean }) {
  return (
    <details className="block search" open={expanded}>
      <summary>
        <span className="block-icon">⌕</span>
        <span className="search-label">
          {call.action.type === 'open_page'
            ? '打开网页'
            : call.action.type === 'find_in_page'
              ? '页内查找'
              : '网络搜索'}
        </span>
        <code className="search-target">
          {call.action.type === 'open_page' ? (
            <a href={call.action.url} target="_blank" rel="noreferrer">
              {call.action.url}
            </a>
          ) : call.action.type === 'search' ? (
            call.query
          ) : (
            call.action.url
          )}
        </code>
        <span className={`search-status ${call.status}`}>{call.status}</span>
      </summary>
      {call.sources && call.sources.length > 0 && (
        <ul className="search-sources">
          {call.sources.map((source, index) => (
            <li key={index}>
              <a href={source.url} target="_blank" rel="noreferrer">
                {source.title}
              </a>
            </li>
          ))}
        </ul>
      )}
    </details>
  )
}

function Citations({ annotations }: { annotations: Annotation[] }) {
  // 与 TUI citation_ref 一致：url_citation 与 web_search_result_location 均展示，按 URL 去重
  type CitedAnnotation = Extract<Annotation, { type: 'url_citation' | 'web_search_result_location' }>
  const seen = new Set<string>()
  const citations = annotations.filter((a): a is CitedAnnotation => {
    if ((a.type !== 'url_citation' && a.type !== 'web_search_result_location') || seen.has(a.url)) return false
    seen.add(a.url)
    return true
  })
  if (citations.length === 0) return null
  return (
    <details className="citations">
      <summary>引用来源（{citations.length}）</summary>
      <ol>
        {citations.map((citation, index) => (
          <li key={index}>
            <a href={citation.url} target="_blank" rel="noreferrer">
              {index + 1}. {citation.title || citation.url}
            </a>
          </li>
        ))}
      </ol>
    </details>
  )
}

export function Part({ part, expanded }: { part: ContentPart; expanded: boolean }) {
  switch (part.type) {
    case 'text':
      return (
        <div className="part-text">
          <Markdown text={part.text} />
          {part.annotations && part.annotations.length > 0 && (
            <Citations annotations={part.annotations} />
          )}
        </div>
      )
    case 'reasoning':
      return (
        <details className="block reasoning" open={expanded}>
          <summary>
            <span className="block-icon">✦</span>
            <span>Thinking</span>
          </summary>
          <div className="reasoning-body">{part.content}</div>
        </details>
      )
    case 'tool_result':
      return (
        <details className="block tool" open={expanded}>
          <summary>
            <span className={`block-icon status ${part.is_error ? 'error' : 'ok'}`}>
              {part.is_error ? '✕' : '✓'}
            </span>
            <span>执行结果</span>
          </summary>
          <pre>{part.content}</pre>
        </details>
      )
    case 'web_search_call':
      return <WebSearchView call={part} expanded={expanded} />
    case 'image':
      return (
        <img
          className="part-image"
          src={
            part.source.kind === 'url'
              ? part.source.url
              : `data:${part.source.mime_type};base64,${part.source.data}`
          }
          alt=""
        />
      )
  }
}

/**
 * live 表示流式生成中：末尾块（思考 / 搜索 / 工具）保持展开，出现后续块或响应结束后自动收起；
 * 错误块不例外，仅摘要行保留 ✕ 标识。历史内容默认收起，用户可手动展开。
 * tool_call 与同 tool_call_id 的 tool_result 配对为一个节点；
 * 未配对的 result（如无调用上下文）独立渲染。
 */
export function Parts({
  parts,
  live = false,
  onPreviewFile,
}: {
  parts: ContentPart[]
  live?: boolean
  onPreviewFile?: (path: string) => void
}) {
  type Node =
    | { kind: 'part'; part: ContentPart }
    | { kind: 'tool'; group: ToolGroupNode }
  const nodes: Node[] = []
  const groupsById = new Map<string, ToolGroupNode>()

  parts.forEach(part => {
    if (part.type === 'tool_call') {
      const group = { call: part, result: null }
      groupsById.set(part.id, group)
      nodes.push({ kind: 'tool', group })
    } else if (part.type === 'tool_result') {
      const group = groupsById.get(part.tool_call_id)
      if (group) {
        group.result = part
      } else {
        nodes.push({ kind: 'part', part })
      }
    } else {
      nodes.push({ kind: 'part', part })
    }
  })

  return (
    <>
      {nodes.map((node, index) => {
        const expanded = live && index === nodes.length - 1
        return node.kind === 'tool' ? (
          <ToolGroupView
            key={`tool-${index}`}
            node={node.group}
            expanded={expanded}
            onPreviewFile={onPreviewFile}
          />
        ) : (
          <Part key={`part-${index}`} part={node.part} expanded={expanded} />
        )
      })}
    </>
  )
}
