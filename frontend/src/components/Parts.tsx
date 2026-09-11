import type { Annotation, ContentPart, ToolCall, ToolResult, WebSearchCall } from '../types'
import { Markdown } from './Markdown'

function prettyJson(raw: string): string {
  try {
    return JSON.stringify(JSON.parse(raw), null, 2)
  } catch {
    return raw
  }
}

function truncate(text: string, max: number): string {
  return text.length > max ? `${text.slice(0, max)}…` : text
}

/** 工具的意图摘要：Shell 用显式 intent 字段，其余工具回退到主要参数 */
function toolIntent(args: string): string {
  try {
    const parsed = JSON.parse(args) as Record<string, unknown>
    if (parsed && typeof parsed === 'object') {
      if (typeof parsed.intent === 'string' && parsed.intent) {
        return parsed.intent
      }
      for (const key of ['command', 'path', 'pattern', 'query', 'url']) {
        const value = parsed[key]
        if (typeof value === 'string' && value) {
          return value
        }
      }
    }
  } catch {
    // 非 JSON 参数时原样截断展示
  }
  return args
}

/** 工具调用与其结果配对后的节点 */
interface ToolGroupNode {
  call: ToolCall
  result: ToolResult | null
}

function ToolGroupView({ node }: { node: ToolGroupNode }) {
  const { call, result } = node
  const intent = truncate(toolIntent(call.arguments), 88)
  return (
    <details className="block tool" open={result?.is_error ?? false}>
      <summary>
        {result ? (
          <span className={`block-icon status ${result.is_error ? 'error' : 'ok'}`}>
            {result.is_error ? '✕' : '✓'}
          </span>
        ) : (
          <span className="block-icon status pending" />
        )}
        <code>{call.name}</code>
        <span className="tool-intent">{intent}</span>
      </summary>
      <pre>{prettyJson(call.arguments)}</pre>
      {result && <pre>{result.content}</pre>}
    </details>
  )
}

function ToolResultView({ content, isError }: { content: string; isError: boolean }) {
  return (
    <details className={`block tool result${isError ? ' error' : ''}`} open={isError}>
      <summary>
        <span className="block-icon">{isError ? '✕' : '✓'}</span>
        <span>{isError ? '执行失败' : '执行结果'}</span>
      </summary>
      <pre>{content}</pre>
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
          {call.action.type === 'search' ? call.query : call.action.url}
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
  const citations = annotations.filter(
    (a): a is Extract<Annotation, { type: 'url_citation' }> => a.type === 'url_citation',
  )
  if (citations.length === 0) return null
  return (
    <div className="citations">
      {citations.map((citation, index) => (
        <a key={index} href={citation.url} target="_blank" rel="noreferrer">
          {index + 1}. {citation.title || citation.url}
        </a>
      ))}
    </div>
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
            <span>思考过程</span>
          </summary>
          <div className="reasoning-body">{part.content}</div>
        </details>
      )
    case 'tool_result':
      return <ToolResultView content={part.content} isError={part.is_error} />
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
 * live 表示流式生成中：末尾块保持展开，出现后续块时自动收起；
 * 历史内容默认收起，用户可手动展开。
 * tool_call 与同 tool_call_id 的 tool_result 配对为一个节点；
 * 未配对的 result（如无调用上下文）独立渲染。
 */
export function Parts({ parts, live = false }: { parts: ContentPart[]; live?: boolean }) {
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
      {nodes.map((node, index) =>
        node.kind === 'tool' ? (
          <ToolGroupView key={`tool-${index}`} node={node.group} />
        ) : (
          <Part
            key={`part-${index}`}
            part={node.part}
            expanded={live && index === nodes.length - 1}
          />
        ),
      )}
    </>
  )
}
