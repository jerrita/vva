import type {
  Annotation,
  ContentPart,
  ImageGenerationCall,
  MediaSource,
  SearchStatus,
  ToolCall,
  ToolResult,
  WebSearchCall,
  WebSearchSource,
} from '../types'
import { IconFile } from './icons'
import { useLightbox } from './Lightbox'
import { Markdown } from './Markdown'

/** 图片数据源地址：URL 直连或 Base64 data URL */
export function mediaSourceUrl(source: MediaSource): string {
  return source.kind === 'url'
    ? source.url
    : `data:${source.mime_type};base64,${source.data}`
}

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

/** Glob/Grep 结果中的匹配行数：非空行且排除截断标记 */
function resultMatchCount(result: ToolResult): number {
  return result.content
    .split('\n')
    .filter(line => line.trim() !== '' && line !== '[results truncated]').length
}

function matchSuffix(count: number): string {
  return `(${count} ${count === 1 ? 'match' : 'matches'})`
}

function toolDetail(call: ToolCall, result: ToolResult | null, workspace?: string): ToolDetail {
  const rel = (path: string) => relativePath(path, workspace)
  const raw = truncate(call.arguments, 88)
  const args = parseArgs(call.arguments)
  if (!args) return { text: raw }
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
      if (!rawPath) return { text: raw }
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
      if (!rawPath) return { text: raw }
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
      const pattern = str(args.pattern) ?? raw
      if (result && !result.is_error) {
        return { text: `"${pattern}" ${matchSuffix(resultMatchCount(result))}` }
      }
      return { text: `"${pattern}"` }
    }
    case 'Grep': {
      const pattern = str(args.pattern)
      if (!pattern) return { text: raw }
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
        return { text: `${target} ${matchSuffix(resultMatchCount(result))}` }
      }
      return { text: target }
    }
    case 'Shell': {
      const command = str(args.command)
      const intent = str(args.intent)
      // 后台启动标注任务态，与前台执行区分
      const background = args.run_in_background === true ? ' (background)' : ''
      return {
        text: (intent ?? '') + background,
        shellCommand: command ?? undefined,
      }
    }
    case 'TaskOutput':
    case 'TaskStop': {
      const taskId = str(args.task_id)
      return { text: taskId ?? raw }
    }
    case 'Agent': {
      const agent = str(args.agent) ?? 'agent'
      const description = str(args.description) ?? ''
      const background = args.run_in_background === true ? ' (background)' : ''
      return { text: description ? `${agent} · ${description}${background}` : `${agent}${background}` }
    }
    case 'SendMessage': {
      const session = str(args.session_id) ?? ''
      const text = str(args.text)
      return { text: text ? `${session} · "${truncate(text, 40)}"` : session }
    }
    case 'WaitSession': {
      const session = str(args.session_id)
      return { text: session ?? raw }
    }
    default:
      return { text: raw }
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
  const isTaskTool = call.name === 'TaskOutput' || call.name === 'TaskStop'
  // 子代理工具：最终消息 / 投递回执可展开
  const isAgentTool = call.name === 'Agent' || call.name === 'SendMessage' || call.name === 'WaitSession'
  // 可展开 = 有输出可看（Shell 输出 / 后台任务输出 / 子代理输出 / 错误详情）
  const hasOutput =
    Boolean((isShell || isTaskTool || isAgentTool) && result && result.content.trim()) ||
    Boolean(isError && result)
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

/**
 * 外置 web_search 工具输出中的编号条目 → sources。
 * 与工具输出格式耦合：`N. title` 行后紧跟 URL 行（title 缺省时该行为 `N. url`）。
 */
function parseWebSources(content: string): WebSearchSource[] {
  const lines = content.split('\n')
  const sources: WebSearchSource[] = []
  for (let index = 0; index < lines.length; index++) {
    const entry = /^\d+\. (.+)$/.exec(lines[index])
    if (!entry) continue
    const candidate = (lines[index + 1] ?? '').trim()
    if (/^https?:\/\//.test(candidate)) {
      sources.push({ url: candidate, title: entry[1] === candidate ? '' : entry[1] })
      index++
    } else if (/^https?:\/\//.test(entry[1])) {
      sources.push({ url: entry[1], title: entry[1] })
    }
  }
  return sources
}

/** 工具执行中/成功/失败 → 原生搜索状态 */
function webToolStatus(result: ToolResult | null): SearchStatus {
  if (!result) return 'searching'
  return result.is_error ? 'failed' : 'completed'
}

/**
 * 外置 web 工具（web_search / web_fetch）→ 原生 WebSearchCall 视图：
 * 与服务端原生搜索共用同一展示形态（见 WebSearchView），其余工具返回 null。
 */
function externalWebSearchCall(node: ToolGroupNode): WebSearchCall | null {
  const { call, result } = node
  const args = parseArgs(call.arguments)
  if (call.name === 'web_search') {
    const query = str(args?.query) ?? ''
    return {
      id: call.id,
      query,
      action: { type: 'search' },
      status: webToolStatus(result),
      sources: result && !result.is_error ? parseWebSources(result.content) : [],
    }
  }
  if (call.name === 'web_fetch') {
    const url = str(args?.url) ?? ''
    if (!url) return null
    return {
      id: call.id,
      query: url,
      action: { type: 'open_page', url },
      status: webToolStatus(result),
      sources: [],
    }
  }
  return null
}

function WebSearchView({ call, expanded }: { call: WebSearchCall; expanded: boolean }) {
  const action = call.action
  return (
    <details className="search-row" open={expanded}>
      <summary>
        <span className="search-icon">⌕</span>
        <span className="search-label">
          {action?.type === 'open_page' ? '打开网页' : action?.type === 'find_in_page' ? '页内查找' : '网络搜索'}
        </span>
        <code className="search-target">
          {action == null ? (
            '（动作未下发）'
          ) : action.type === 'open_page' ? (
            <a href={action.url} target="_blank" rel="noreferrer">
              {action.url}
            </a>
          ) : action.type === 'search' ? (
            call.query
          ) : (
            action.url
          )}
        </code>
        <span className={`search-status ${call.status}`}>{call.status}</span>
      </summary>
      {call.sources && call.sources.length > 0 && (
        <ul className="search-sources">
          {call.sources.map((source, index) => (
            <li key={index}>
              <a href={source.url} target="_blank" rel="noreferrer">
                {source.title || source.url}
              </a>
            </li>
          ))}
        </ul>
      )}
    </details>
  )
}

function ImageGenerationView({ call, expanded }: { call: ImageGenerationCall; expanded: boolean }) {
  const openLightbox = useLightbox()
  return (
    <details className="search-row image-gen-row" open={expanded}>
      <summary>
        <span className="search-icon">▦</span>
        <span className="search-label">生成图片</span>
        <code className="search-target">{call.revised_prompt ?? ''}</code>
        <span className={`search-status ${call.status}`}>{call.status}</span>
      </summary>
      {call.result && (
        <img
          className="part-image lightboxable"
          src={mediaSourceUrl(call.result)}
          alt=""
          onClick={() => openLightbox(mediaSourceUrl(call.result!))}
        />
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
  const openLightbox = useLightbox()
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
    case 'image_generation_call':
      return <ImageGenerationView call={part} expanded={expanded} />
    case 'image': {
      const url = mediaSourceUrl(part.source)
      return (
        <img
          className="part-image lightboxable"
          src={url}
          alt=""
          onClick={() => openLightbox(url)}
        />
      )
    }
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
        const webCall = node.kind === 'tool' ? externalWebSearchCall(node.group) : null
        if (webCall) {
          return <WebSearchView key={`tool-${index}`} call={webCall} expanded={expanded} />
        }
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
