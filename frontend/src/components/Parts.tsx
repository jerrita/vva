import type { Annotation, ContentPart, ToolCall, ToolResult, WebSearchCall } from '../types'
import { IconFile } from './icons'
import { Markdown } from './Markdown'

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

function fileName(path: string): string {
  const filePath = path.split('#', 1)[0]
  return filePath.slice(filePath.lastIndexOf('/') + 1)
}

/** 项目内文件的展示路径相对化；项目外保持原样 */
function relativePath(path: string, workspace?: string): string {
  if (!workspace) return path
  const root = workspace.replace(/\/+$/, '')
  if (path === root) return '.'
  if (path.startsWith(`${root}/`)) return path.slice(root.length + 1)
  return path
}

function patchFilePath(input: string): string | null {
  for (const line of input.split('\n')) {
    for (const prefix of ['*** Update File: ', '*** Add File: ', '*** Delete File: ']) {
      if (line.startsWith(prefix)) return line.slice(prefix.length).trim()
    }
  }
  return null
}

function patchCounts(input: string): { plus: number; minus: number } {
  let plus = 0
  let minus = 0
  for (const line of input.split('\n')) {
    const trimmed = line.replace(/\s+$/, '')
    if (trimmed.startsWith('***') || trimmed.startsWith('@@')) continue
    if (trimmed.startsWith('+')) plus += 1
    else if (trimmed.startsWith('-')) minus += 1
  }
  return { plus, minus }
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
  /** 原始路径：预览跳转用 */
  filePath?: string
  /** 展示路径：项目内文件相对化 */
  displayPath?: string
  /** Edit 专用：增删行数着色展示 */
  editPath?: string
  plus?: number
  minus?: number
  shellCommand?: string
}

function toolDetail(call: ToolCall, result: ToolResult | null, workspace?: string): ToolDetail {
  const rel = (path: string) => relativePath(path, workspace)
  const args = parseArgs(call.arguments)
  if (!args) return { text: truncate(call.arguments, 88) }
  switch (call.name) {
    case 'AskUser': {
      const questions = Array.isArray(args.questions) ? args.questions : []
      if (questions.length !== 1) return { text: `${questions.length} questions` }
      const question = questions[0]
      if (!question || typeof question !== 'object') return { text: 'user input' }
      const item = question as Record<string, unknown>
      const header = str(item.header)
      const mode = item.multi_select === true ? '多选' : '单选'
      return { text: header ? `${header} · ${mode}` : mode }
    }
    case 'Read': {
      const rawPath = str(args.path)
      if (!rawPath) return { text: truncate(call.arguments, 88) }
      const options: string[] = []
      if (typeof args.offset === 'number' && args.offset !== 1) options.push(`offset=${args.offset}`)
      if (typeof args.limit === 'number') options.push(`limit=${args.limit}`)
      if (typeof args.byte_offset === 'number') options.push(`byte_offset=${args.byte_offset}`)
      const shown = rel(rawPath)
      return {
        text: options.length > 0 ? `${shown} [${options.join(', ')}]` : shown,
        filePath: rawPath,
        displayPath: shown,
      }
    }
    case 'Write': {
      const rawPath = str(args.path)
      if (!rawPath) return { text: truncate(call.arguments, 88) }
      return { text: rel(rawPath), filePath: rawPath, displayPath: rel(rawPath) }
    }
    case 'Edit': {
      const input = str(args.input)
      if (input) {
        const path = patchFilePath(input)
        const { plus, minus } = patchCounts(input)
        if (path) {
          return { text: fileName(path), filePath: path, editPath: rel(path), plus, minus }
        }
        return { text: truncate(input, 88) }
      }
      const path = str(args.path)
      const edits = editEntries(args)
      if (path && edits.length > 0 && edits.every(edit => str(edit.old_string) !== null && str(edit.new_string) !== null)) {
        return {
          text: fileName(path),
          filePath: path,
          editPath: rel(path),
          plus: edits.reduce((count, edit) => count + lineCount(str(edit.new_string)!), 0),
          minus: edits.reduce((count, edit) => count + lineCount(str(edit.old_string)!), 0),
        }
      }
      return { text: rel(path ?? call.arguments), filePath: path ?? undefined, displayPath: path ? rel(path) : undefined }
    }
    case 'Glob': {
      const pattern = str(args.pattern) ?? truncate(call.arguments, 88)
      if (result && !result.is_error) {
        const count = result.content
          .split('\n')
          .filter(line => line.trim() !== '' && line !== '[results truncated]').length
        const noun = count === 1 ? 'match' : 'matches'
        return { text: `"${pattern}" (${count} ${noun})` }
      }
      return { text: `"${pattern}"` }
    }
    case 'Grep': {
      const pattern = str(args.pattern)
      if (!pattern) return { text: truncate(call.arguments, 88) }
      const include = str(args.include)
      const path = str(args.path)
      const shown = path ? rel(path) : null
      const loc = include
        ? shown && shown !== '.'
          ? `${shown}/${include}`
          : include
        : shown && shown !== '.'
          ? shown
          : null
      const target = loc ? `${loc}, "${pattern}"` : `"${pattern}"`
      if (result && !result.is_error) {
        const count = result.content
          .split('\n')
          .filter(line => line.trim() !== '' && line !== '[results truncated]').length
        const noun = count === 1 ? 'match' : 'matches'
        return { text: `${target} (${count} ${noun})` }
      }
      return { text: target }
    }
    case 'Shell': {
      const command = str(args.command)
      const intent = str(args.intent)
      return {
        text: intent ?? '',
        shellCommand: command ?? undefined,
      }
    }
    default:
      return { text: truncate(call.arguments, 88) }
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
  workspace,
  onPreviewFile,
}: {
  node: ToolGroupNode
  expanded?: boolean
  workspace?: string
  onPreviewFile?: (path: string) => void
}) {
  const { call, result } = node
  const detail = toolDetail(call, result, workspace)
  const isError = result?.is_error ?? false
  const isFileTool = call.name === 'Read' || call.name === 'Write' || call.name === 'Edit'
  const isShell = call.name === 'Shell'
  // 可展开 = 有输出可看（Shell 输出 / 错误详情）
  const hasOutput = Boolean(isShell && result && result.content.trim()) || Boolean(isError && result)
  const shownPath = detail.displayPath ?? detail.filePath

  if (!hasOutput) {
    return (
      <div
        className={`tool-row${isError ? ' error' : ''}${
          isFileTool && detail.filePath && onPreviewFile ? ' clickable' : ''
        }`}
        onClick={
          isFileTool && detail.filePath && onPreviewFile
            ? () => onPreviewFile(detail.filePath!)
            : undefined
        }
        title={
          isFileTool && detail.filePath
            ? `在右侧边栏预览 ${detail.filePath}`
            : undefined
        }
      >
        <code className="tool-name">{call.name}</code>
        {detail.editPath ? (
          <span className="tool-intent">
            <span className="file-name">{detail.editPath}</span>{' '}
            <span className="diff-count ins">(+{detail.plus})</span>
            {', '}
            <span className="diff-count del">(-{detail.minus})</span>
          </span>
        ) : shownPath ? (
          <span className="tool-intent">
            <span className="file-name">{shownPath}</span>
            {detail.text.slice(shownPath.length)}
          </span>
        ) : (
          <span className="tool-intent">{detail.text}</span>
        )}
        {isFileTool && detail.filePath && onPreviewFile && (
          <span className="tool-preview-hint" title="预览文件" aria-hidden>
            <IconFile size={13} strokeWidth={1.8} />
          </span>
        )}
      </div>
    )
  }

  return (
    <details className={`tool-row${isError ? ' error' : ''}`} open={expanded}>
      <summary>
        <code className="tool-name">{call.name}</code>
        <span className="tool-intent">{detail.text}</span>
      </summary>
      <pre className="tool-output">
        {detail.shellCommand && <span className="tool-command">{detail.shellCommand}</span>}
        {result!.content}
      </pre>
    </details>
  )
}

function WebSearchView({ call, expanded }: { call: WebSearchCall; expanded: boolean }) {
  return (
    <details className="search-row" open={expanded}>
      <summary>
        <span className="search-icon">⌕</span>
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
        <details className="reasoning-row" open={expanded}>
          <summary>
            <span className="reasoning-icon">✦</span>
            <span className="reasoning-label">Thinking</span>
          </summary>
          <div className="reasoning-body">{part.content}</div>
        </details>
      )
    case 'tool_result':
      return (
        <details className={`tool-row${part.is_error ? ' error' : ''}`} open={expanded}>
          <summary>
            <code className="tool-name">result</code>
            <span className="tool-intent">{part.is_error ? '执行出错' : '执行结果'}</span>
          </summary>
          <pre className="tool-output">{part.content}</pre>
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
 * tool_call 与同 tool_call_id 的 tool_result 配对为一个节点；
 * 未配对的 result（如无调用上下文）独立渲染。
 * 历史内容默认收起，用户可手动展开。
 */
export function Parts({
  parts,
  live = false,
  workspace,
  onPreviewFile,
}: {
  parts: ContentPart[]
  live?: boolean
  /** 会话 workspace：项目内文件展示为相对路径 */
  workspace?: string
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
            workspace={workspace}
            onPreviewFile={onPreviewFile}
          />
        ) : (
          <Part key={`part-${index}`} part={node.part} expanded={expanded} />
        )
      })}
    </>
  )
}
