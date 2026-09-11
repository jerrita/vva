import type { ContentPart, Session, Turn } from '../types'
import { Parts } from './Parts'

export function turnText(turn: Turn): string {
  return turn.messages
    .map(message =>
      message.parts
        .filter(part => part.type === 'text')
        .map(part => part.text)
        .join(''),
    )
    .join('')
}

/** AGENTS.md 等系统注入的提醒轮次不在聊天界面展示 */
function isSystemReminder(turn: Turn): boolean {
  if (turn.role !== 'user') return false
  const text = turnText(turn).trim()
  return text.startsWith('<system-reminder') && text.endsWith('</system-reminder>')
}

const isWorkPart = (part: ContentPart) =>
  part.type === 'reasoning' ||
  part.type === 'web_search_call' ||
  part.type === 'tool_call' ||
  part.type === 'tool_result'

/** 历史轮次中的连续工作段（思考/搜索/工具），折叠为一个 Worked 块 */
interface WorkGroup {
  parts: ContentPart[]
  /** 各响应耗时之和 */
  elapsedMs: number | null
  /** 整个请求周期的总耗时（最终响应携带） */
  workedMs: number | null
  searches: number
  tools: number
  errors: number
}

type Item =
  | { kind: 'user'; turn: Turn }
  | { kind: 'work'; group: WorkGroup }
  | { kind: 'assistant'; turn: Turn; parts: ContentPart[] }

function buildItems(turns: Turn[]): Item[] {
  const items: Item[] = []
  let group: WorkGroup | null = null
  const flush = () => {
    if (group && group.parts.length > 0) items.push({ kind: 'work', group })
    group = null
  }
  for (const turn of turns) {
    if (turn.role === 'user') {
      flush()
      items.push({ kind: 'user', turn })
      continue
    }
    const parts = turn.messages.flatMap(message => message.parts)
    const work = parts.filter(isWorkPart)
    const text = parts.filter(part => !isWorkPart(part))
    if (work.length > 0) {
      group ??= {
        parts: [],
        elapsedMs: null,
        workedMs: null,
        searches: 0,
        tools: 0,
        errors: 0,
      }
      for (const part of work) {
        group.parts.push(part)
        if (part.type === 'web_search_call') group.searches++
        if (part.type === 'tool_call') group.tools++
        if (part.type === 'tool_result' && part.is_error) group.errors++
      }
      if (turn.elapsed_ms != null) {
        group.elapsedMs = (group.elapsedMs ?? 0) + turn.elapsed_ms
      }
      if (turn.worked_ms != null) {
        group.workedMs = turn.worked_ms
      }
    }
    if (text.length > 0) {
      flush()
      items.push({ kind: 'assistant', turn, parts: text })
    }
  }
  flush()
  return items
}

function WorkedBlock({ group }: { group: WorkGroup }) {
  const ms = group.workedMs ?? group.elapsedMs
  const bits: string[] = []
  if (group.searches > 0) bits.push(`${group.searches} 次搜索`)
  if (group.tools > 0) bits.push(`${group.tools} 个工具`)
  return (
    <details className="block worked" open={group.errors > 0}>
      <summary>
        <span className="block-icon">⚒</span>
        <span>{ms != null ? `Worked ${(ms / 1000).toFixed(1)}s` : 'Worked'}</span>
        {bits.length > 0 && <span className="worked-bits">{bits.join(' · ')}</span>}
      </summary>
      <div className="worked-body">
        <Parts parts={group.parts} />
      </div>
    </details>
  )
}

function TurnMeta({ turn }: { turn: Turn }) {
  const bits: string[] = []
  if (turn.worked_ms != null) {
    bits.push(`${(turn.worked_ms / 1000).toFixed(1)}s`)
  } else if (turn.elapsed_ms != null) {
    bits.push(`${(turn.elapsed_ms / 1000).toFixed(1)}s`)
  }
  if (turn.usage) {
    bits.push(`${turn.usage.total_tokens.toLocaleString()} tokens`)
  }
  if (bits.length === 0) return null
  return <div className="turn-meta">{bits.join(' · ')}</div>
}

function UserBubble({ turn }: { turn: Turn }) {
  return (
    <div className="msg user">
      <div className="bubble">{turnText(turn)}</div>
    </div>
  )
}

export function Chat({
  snap,
  pending,
}: {
  snap: { state: string; draft: ContentPart[]; session: Session } | null
  pending: string | null
}) {
  if (!snap) {
    return (
      <div className="chat-empty">
        <div className="chat-empty-brand">
          vva
          <span className="brand-cursor" aria-hidden />
        </div>
        <div className="chat-empty-hint">点击左侧「新对话」开始</div>
      </div>
    )
  }

  const running = snap.state !== ''
  const turns = snap.session.turns.filter(turn => turn.role !== 'system' && !isSystemReminder(turn))
  const showDraft = snap.draft.length > 0 || running

  return (
    <div className="chat">
      {buildItems(turns).map((item, index) => {
        if (item.kind === 'user') {
          return <UserBubble key={item.turn.id} turn={item.turn} />
        }
        if (item.kind === 'work') {
          return <WorkedBlock key={`work-${index}`} group={item.group} />
        }
        return (
          <div key={item.turn.id} className="msg assistant">
            <Parts parts={item.parts} />
            <TurnMeta turn={item.turn} />
          </div>
        )
      })}
      {pending && <UserBubble turn={{ id: 'pending', role: 'user', messages: [{ parts: [{ type: 'text', text: pending }] }] }} />}
      {showDraft && (
        <div className="msg assistant">
          <Parts parts={snap.draft} live={running} />
          {running && (
            <div className="status-chip">
              <span className="dot" />
              {snap.state}
            </div>
          )}
        </div>
      )}
    </div>
  )
}
