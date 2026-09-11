import type { Annotation, ContentPart, WebSearchCall } from '../types'
import { Markdown } from './Markdown'

function prettyJson(raw: string): string {
  try {
    return JSON.stringify(JSON.parse(raw), null, 2)
  } catch {
    return raw
  }
}

function ToolCallView({ name, arguments: args }: { name: string; arguments: string }) {
  return (
    <details className="block tool">
      <summary>
        <span className="block-icon">⚙</span>
        <code>{name}</code>
      </summary>
      <pre>{prettyJson(args)}</pre>
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
    case 'tool_call':
      return <ToolCallView name={part.name} arguments={part.arguments} />
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
 */
export function Parts({ parts, live = false }: { parts: ContentPart[]; live?: boolean }) {
  return (
    <>
      {parts.map((part, index) => (
        <Part key={index} part={part} expanded={live && index === parts.length - 1} />
      ))}
    </>
  )
}
