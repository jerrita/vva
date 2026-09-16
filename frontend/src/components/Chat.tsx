import type { ContentPart, Session, Turn } from '../types'
import { CopyButton } from './CopyButton'
import { IconFork, IconRevert } from './icons'
import { Parts, mediaSourceUrl } from './Parts'

/** 部件列表中的纯文本（消息原文与复制回答用） */
export function partsText(parts: ContentPart[]): string {
  return parts
    .filter(part => part.type === 'text')
    .map(part => part.text)
    .join('')
}

export function turnText(turn: Turn): string {
  return partsText(turn.messages.flatMap(message => message.parts))
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
    group ??= { parts: [], elapsedMs: null, workedMs: null }
    for (const part of parts) {
      group.parts.push(part)
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
    // 末段文本 = 最终回答；思考/搜索/工具及其前导过渡文本并入折叠组
    let lastTextIndex = -1
    for (let i = parts.length - 1; i >= 0; i--) {
      if (parts[i].type === 'text') {
        lastTextIndex = i
        break
      }
    }

    if (lastTextIndex === -1) {
      pushTurn(turn, parts)
    } else {
      let answerStart = lastTextIndex
      while (answerStart > 0 && parts[answerStart - 1].type === 'text') {
        answerStart--
      }
      const workParts: ContentPart[] = []
      const answerParts: ContentPart[] = []
      for (let i = 0; i < parts.length; i++) {
        if (i >= answerStart && i <= lastTextIndex) {
          answerParts.push(parts[i])
        } else {
          workParts.push(parts[i])
        }
      }
      if (workParts.length > 0) {
        pushTurn(turn, workParts)
      }
      if (answerParts.length > 0) {
        flush()
        items.push({ kind: 'assistant', turn, parts: answerParts })
      }
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

function WorkedBlock({
  group,
  live = false,
  autoExpand = false,
  workspace,
  onPreviewFile,
}: {
  group: WorkGroup
  live?: boolean
  /** 草稿为空的间隙（如工具执行中）当前块仍在工作段内：末尾块保持展开 */
  autoExpand?: boolean
  workspace?: string
  onPreviewFile?: (path: string) => void
}) {
  const ms = group.workedMs ?? group.elapsedMs
  return (
    <details className="worked" open={live}>
      <summary>{ms != null ? `用时 ${fmtDuration(ms)}` : '执行过程'}</summary>
      <div className="worked-body">
        <Parts parts={group.parts} live={autoExpand} workspace={workspace} onPreviewFile={onPreviewFile} />
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
  onPreviewFile,
}: {
  turn: Turn
  /** 轮次在 session.turns 中的原始下标；乐观展示的 pending 消息无下标、无操作 */
  index?: number
  disabled?: boolean
  onRevert?: (index: number, prompt: string) => void
  onFork?: (index: number) => void
  onPreviewFile?: (path: string) => void
}) {
  const actions = (
    <div className="turn-actions">
      <CopyButton text={turnText(turn)} />
      {index != null && onRevert && onFork && (
        <>
          <button
            disabled={disabled}
            onClick={() => onRevert(index, turnText(turn))}
            title="回退：丢弃此消息及之后的内容，放回输入框重新编辑"
            aria-label="回退"
          >
            <IconRevert size={12} />
          </button>
          <button
            disabled={disabled}
            onClick={() => onFork(index)}
            title="分叉：保留此消息之前的历史，创建新会话"
            aria-label="分叉"
          >
            <IconFork size={12} />
          </button>
        </>
      )}
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
                    src={mediaSourceUrl(part.source)}
                    alt=""
                  />
                )
              }
              if (part.type === 'file') {
                return onPreviewFile ? (
                  <button
                    key={index}
                    type="button"
                    className="bubble-file clickable"
                    onClick={() => onPreviewFile(part.filename)}
                    title="在右侧边栏预览文件"
                  >
                    📄 {part.filename}
                  </button>
                ) : (
                  <span key={index} className="bubble-file">
                    📄 {part.filename}
                  </span>
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
  onPreviewFile,
}: {
  snap: { state: string; draft: ContentPart[]; session: Session } | null
  pending: string | null
  onRevert?: (index: number, prompt: string) => void
  onFork?: (index: number) => void
  onPreviewFile?: (path: string) => void
}) {
  if (!snap) {
    return (
      <div className="chat-empty">
        <div className="empty-brand">vva</div>
        <div className="empty-hint">点击左侧「新对话」开始</div>
      </div>
    )
  }

  const running = snap.state !== ''
  const turns = snap.session.turns
  const workspace = snap.session.workspace_root
  const showDraft = snap.draft.length > 0 || running
  // 运行中最后一个工作段即当前响应段：容器保持展开，结束后随 live 翻转自动收起
  const items = buildItems(turns)
  let liveWork = -1
  if (running) {
    // 仅当前运行段（最后一条用户消息之后、且无最终回答收尾）的工作段视为 live：
    // 新消息的 pending 气泡尚未进入 turns，此时 items 末尾仍是上一轮的 work + answer，
    // 遇 user 或 answer（运行段收尾）即停止，避免把上一轮工作段展开
    for (let index = items.length - 1; index >= 0; index--) {
      const kind = items[index].kind
      if (kind === 'user' || kind === 'assistant') break
      if (kind === 'work') {
        liveWork = index
        break
      }
    }
  }
  // 末尾展开块全局唯一：草稿持有流式块时，工作段不再保留末尾展开块
  const draftActive = running && snap.draft.length > 0

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
              onPreviewFile={onPreviewFile}
            />
          )
        }
        if (item.kind === 'work') {
          return (
            <WorkedBlock
              key={`work-${index}`}
              group={item.group}
              live={index === liveWork}
              autoExpand={index === liveWork && !draftActive}
              workspace={workspace}
              onPreviewFile={onPreviewFile}
            />
          )
        }
        // 仅有收折的工作段在前时才绘制分隔线
        const hasWorkBefore = index > 0 && items[index - 1].kind === 'work'
        return (
          <div
            key={item.turn.id}
            className={`msg assistant answer${hasWorkBefore ? '' : ' no-work'}`}
          >
            <Parts parts={item.parts} workspace={workspace} onPreviewFile={onPreviewFile} />
            <div className="answer-foot">
              <TurnMeta turn={item.turn} />
              <div className="turn-actions">
                <CopyButton text={partsText(item.parts)} />
              </div>
            </div>
          </div>
        )
      })}
      {pending && <UserBubble turn={{ id: 'pending', role: 'user', messages: [{ parts: [{ type: 'text', text: pending }] }] }} onPreviewFile={onPreviewFile} />}
      {showDraft && (
        <div className="msg assistant">
          <Parts parts={snap.draft} live={running} workspace={workspace} onPreviewFile={onPreviewFile} />
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
