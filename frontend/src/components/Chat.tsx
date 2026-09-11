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

function TurnView({ turn }: { turn: Turn }) {
  if (turn.role === 'user') {
    return (
      <div className="msg user">
        <div className="bubble">{turnText(turn)}</div>
      </div>
    )
  }
  if (turn.role === 'assistant') {
    return (
      <div className="msg assistant">
        <Parts parts={turn.messages.flatMap(message => message.parts)} />
        <TurnMeta turn={turn} />
      </div>
    )
  }
  return (
    <div className="msg tool">
      <Parts parts={turn.messages.flatMap(message => message.parts)} />
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
        <div className="chat-empty-brand">vva</div>
        <div className="chat-empty-hint">点击左侧「新对话」开始</div>
      </div>
    )
  }

  const running = snap.state !== ''
  const turns = snap.session.turns.filter(turn => turn.role !== 'system' && !isSystemReminder(turn))
  const showDraft = snap.draft.length > 0 || running

  return (
    <div className="chat">
      {turns.map(turn => (
        <TurnView key={turn.id} turn={turn} />
      ))}
      {pending && (
        <div className="msg user">
          <div className="bubble">{pending}</div>
        </div>
      )}
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
