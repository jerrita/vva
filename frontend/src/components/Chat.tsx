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

/** 历史轮次中的连续工作段（思考/搜索/工具与中间轮文本），折叠为一个用时行 */
interface WorkGroup {
  parts: ContentPart[]
  /** 各响应耗时之和 */
  elapsedMs: number | null
  /** 整个请求周期的总耗时（最终响应携带） */
  workedMs: number | null
  errors: number
}

type Item =
  | { kind: 'user'; turn: Turn; index: number }
  | { kind: 'work'; group: WorkGroup }
  | { kind: 'assistant'; turn: Turn; parts: ContentPart[] }

function buildItems(turns: Turn[]): Item[] {
  const items: Item[] = []
  let group: WorkGroup | null = null
  const flush = () => {
    if (group && group.parts.length > 0) items.push({ kind: 'work', group })
    group = null
  }
  const pushTurn = (turn: Turn, parts: ContentPart[]) => {
    group ??= { parts: [], elapsedMs: null, workedMs: null, errors: 0 }
    for (const part of parts) {
      group.parts.push(part)
      if (part.type === 'tool_result' && part.is_error) group.errors++
    }
    if (turn.elapsed_ms != null) {
      group.elapsedMs = (group.elapsedMs ?? 0) + turn.elapsed_ms
    }
    if (turn.worked_ms != null) {
      group.workedMs = turn.worked_ms
    }
  }
  for (let index = 0; index < turns.length; index++) {
    const turn = turns[index]
    if (turn.role === 'system' || isSystemReminder(turn)) continue
    if (turn.role === 'user') {
      flush()
      items.push({ kind: 'user', turn, index })
      continue
    }
    const parts = turn.messages.flatMap(message => message.parts)
    // 运行段 = 相邻助手轮直到用户消息；折叠折到运行段最后一条消息，
    // 中间轮的思考/工具与文本均并入折叠组，不单独展示
    const next = turns[index + 1]
    const isRunEnd = next == null || next.role === 'user'
    if (!isRunEnd) {
      pushTurn(turn, parts)
      continue
    }
    // 末段连续文本 = 最终回答；其余部分（思考/搜索/工具及其前导过渡文本）
    // 按原位顺序并入折叠组，保持与模型输出一致的块序
    let answerStart = parts.length
    while (answerStart > 0 && parts[answerStart - 1].type === 'text') answerStart--
    if (answerStart > 0) pushTurn(turn, parts.slice(0, answerStart))
    if (answerStart < parts.length) {
      flush()
      items.push({ kind: 'assistant', turn, parts: parts.slice(answerStart) })
    }
  }
  flush()
  return items
}

function fmtDuration(ms: number): string {
  const total = Math.round(ms / 1000)
  if (total < 60) return `${total}s`
  return `${Math.floor(total / 60)}m ${total % 60}s`
}

function WorkedBlock({ group, live = false }: { group: WorkGroup; live?: boolean }) {
  const ms = group.workedMs ?? group.elapsedMs
  return (
    <details className="worked" open={live || group.errors > 0}>
      <summary>{ms != null ? `用时 ${fmtDuration(ms)}` : '执行过程'}</summary>
      <div className="worked-body">
        <Parts parts={group.parts} live={live} />
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

function UserBubble({
  turn,
  index,
  disabled,
  onRevert,
  onFork,
}: {
  turn: Turn
  /** 轮次在 session.turns 中的原始下标；乐观展示的 pending 消息无下标、无操作 */
  index?: number
  disabled?: boolean
  onRevert?: (index: number, prompt: string) => void
  onFork?: (index: number) => void
}) {
  const actions =
    index == null || !onRevert || !onFork ? null : (
      <div className="turn-actions">
        <button
          disabled={disabled}
          onClick={() => onRevert(index, turnText(turn))}
          title="回退：丢弃此消息及之后的内容，放回输入框重新编辑"
          aria-label="回退"
        >
          <svg
            viewBox="0 0 24 24"
            width="12"
            height="12"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
            aria-hidden
          >
            <path d="M9 14 4 9l5-5" />
            <path d="M4 9h10.5a5.5 5.5 0 0 1 0 11H11" />
          </svg>
        </button>
        <button
          disabled={disabled}
          onClick={() => onFork(index)}
          title="分叉：保留此消息之前的历史，创建新会话"
          aria-label="分叉"
        >
          <svg
            viewBox="0 0 24 24"
            width="12"
            height="12"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
            aria-hidden
          >
            <line x1="6" x2="6" y1="3" y2="15" />
            <circle cx="18" cy="6" r="3" />
            <circle cx="6" cy="18" r="3" />
            <path d="M18 9a9 9 0 0 1-9 9" />
          </svg>
        </button>
      </div>
    )
  return (
    <div className="msg user">
      <div className="user-col">
        <div className="bubble">
          {turn.messages
            .flatMap(message => message.parts)
            .map((part, index) => {
              if (part.type === 'image') {
                return (
                  <img
                    key={index}
                    className="bubble-img"
                    src={
                      part.source.kind === 'url'
                        ? part.source.url
                        : `data:${part.source.mime_type};base64,${part.source.data}`
                    }
                    alt=""
                  />
                )
              }
              if (part.type === 'text') {
                return <span key={index}>{part.text}</span>
              }
              return null
            })}
        </div>
        {actions}
      </div>
    </div>
  )
}

export function Chat({
  snap,
  pending,
  onRevert,
  onFork,
}: {
  snap: { state: string; draft: ContentPart[]; session: Session } | null
  pending: string | null
  onRevert?: (index: number, prompt: string) => void
  onFork?: (index: number) => void
}) {
  if (!snap) {
    return (
      <div className="chat-empty">
      <div className="chat-empty-brand">vva</div>
        <div className="chat-empty-hint">点击左侧「新对话」开始</div>
      </div>
    )
  }

  const running = snap.state !== ''
  const turns = snap.session.turns
  const showDraft = snap.draft.length > 0 || running
  // 运行中最后一个工作段即当前响应段：保持展开，结束后随 live 翻转自动收起
  const items = buildItems(turns)
  let liveWork = -1
  if (running) {
    for (let index = items.length - 1; index >= 0; index--) {
      if (items[index].kind === 'work') {
        liveWork = index
        break
      }
    }
  }

  return (
    <div className="chat">
      {items.map((item, index) => {
        if (item.kind === 'user') {
          return (
            <UserBubble
              key={item.turn.id}
              turn={item.turn}
              index={item.index}
              disabled={running}
              onRevert={onRevert}
              onFork={onFork}
            />
          )
        }
        if (item.kind === 'work') {
          return <WorkedBlock key={`work-${index}`} group={item.group} live={index === liveWork} />
        }
        return (
          <div key={item.turn.id} className="msg assistant answer">
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
