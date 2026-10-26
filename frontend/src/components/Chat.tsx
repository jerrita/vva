import { memo, useEffect, useRef, useMemo } from 'react'
import type { PreviewFile } from '../hooks/usePanelLayout'
import type { BackgroundTask, ContentPart, Session, Turn, ShellOutput } from '../types'
import { taskTrace } from '../utils/taskTrace'
import type { TaskTrace } from '../utils/taskTrace'
import { CopyButton } from './CopyButton'
import { IconFile, IconFork, IconRevert } from './icons'
import { useLightbox } from './Lightbox'
import { Parts, mediaSourceUrl, tallyParts } from './Parts'

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

/** 机器注入轮次的 system-reminder type 分类；非 reminder 用户消息返回 null */
function reminderType(turn: Turn): string | null {
  if (turn.role !== 'user') return null
  const text = turnText(turn).trim()
  if (!text.startsWith('<system-reminder') || !text.endsWith('</system-reminder>')) return null
  const openTag = text.slice(0, text.indexOf('>'))
  return /type="([^"]*)"/.exec(openTag)?.[1] ?? null
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
    if (turn.role === 'system') continue
    if (turn.role === 'user') {
      if (reminderType(turn) !== null) {
        // 机器注入轮次不渲染；后台完成状态由原工具块承载。
        // 轮隙不切断运行段折叠
        continue
      }
      flush()
      items.push({ kind: 'user', turn, index })
      continue
    }
    const parts = turn.messages.flatMap(message => message.parts)
    // 运行段 = 相邻助手轮直到用户消息；折叠折到运行段最后一条消息，
    // 中间轮的思考/工具与文本均并入折叠组，不单独展示。
    // 本轮带 tool_call 时后续必有工具结果，即使结果尚未落盘也不拆尾部文本。
    let nextIndex = index + 1
    let awaitingContinuation = false
    while (nextIndex < turns.length) {
      const next = turns[nextIndex]
      if (next.role === 'system') {
        nextIndex++
      } else if (reminderType(next) !== null) {
        awaitingContinuation = true
        nextIndex++
      } else {
        break
      }
    }
    const next = turns[nextIndex]
    // reminder 隐藏后仍属于当前运行段；仅收到通知时，前次回答先并入工作段等待续接。
    const isRunEnd = next == null ? !awaitingContinuation : next.role === 'user'
    const pendingTools = parts.some(part => part.type === 'tool_call')
    if (!isRunEnd || pendingTools) {
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

    if (lastTextIndex === -1 && !parts.some(part => part.type === 'image_generation_call')) {
      pushTurn(turn, parts)
    } else {
      let answerStart = lastTextIndex
      while (answerStart > 0 && parts[answerStart - 1].type === 'text') {
        answerStart--
      }
      const workParts: ContentPart[] = []
      const answerParts: ContentPart[] = []
      for (let i = 0; i < parts.length; i++) {
        const part = parts[i]
        if (i >= answerStart && i <= lastTextIndex) {
          answerParts.push(part)
        } else if (part.type === 'image_generation_call' && part.result) {
          // 生图调用折叠进执行过程，其结果图在最终回答中再展示一遍
          workParts.push(part)
          answerParts.push({ type: 'image', source: part.result })
        } else {
          workParts.push(part)
        }
      }
      pushTurn(turn, workParts)
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

const WorkedBlock = memo(function WorkedBlock({
  group,
  live = false,
  workspace,
  onPreviewFile,
  tasks,
}: {
  group: WorkGroup
  live?: boolean
  workspace?: string
  onPreviewFile?: PreviewFile
  tasks: TaskTrace
}) {
  const ms = group.workedMs ?? group.elapsedMs
  const tally = tallyParts(group.parts, tasks)
  const details = useRef<HTMLDetailsElement>(null)
  // 仅主轮次切换改变自动折叠；后台状态更新保留用户的展开选择。
  useEffect(() => {
    if (details.current) details.current.open = live
  }, [live])
  return (
    <details className="worked" ref={details}>
      <summary>
        <span className="worked-time">{live ? '进行中' : ms != null ? `用时 ${fmtDuration(ms)}` : '执行过程'}</span>
        {tally.counts.map(([key, count]) => (
          <span key={key} className="tally">
            {key} <b>{count}</b>
          </span>
        ))}
        {tally.failed > 0 && (
          <span className="tally fail">
            失败 <b>{tally.failed}</b>
          </span>
        )}
        {tally.backgroundRunning > 0 && (
          <span className="tally background-running">后台运行 <b>{tally.backgroundRunning}</b></span>
        )}
      </summary>
      <div className="worked-body">
        <Parts parts={group.parts} live={live} workspace={workspace} onPreviewFile={onPreviewFile} tasks={tasks} />
      </div>
    </details>
  )
}, (a, b) => {
  if (a.group !== b.group || a.live !== b.live || a.workspace !== b.workspace ||
    a.onPreviewFile !== b.onPreviewFile || a.tasks.tasks !== b.tasks.tasks ||
    a.tasks.queries !== b.tasks.queries) return false
  return a.group.parts.every(part => part.type !== 'tool_call' ||
    a.tasks.shellOutputs?.[part.id] === b.tasks.shellOutputs?.[part.id])
})


function TurnMeta({ turn }: { turn: Turn }) {
  const ms = turn.worked_ms ?? turn.elapsed_ms
  if (ms == null && !turn.usage) return null
  return (
    <div className="turn-meta">
      {ms != null && <span>{(ms / 1000).toFixed(1)}s</span>}
      {turn.usage && <span>{turn.usage.total_tokens.toLocaleString()} tokens</span>}
    </div>
  )
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
  onPreviewFile?: PreviewFile
}) {
  const openLightbox = useLightbox()
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
                const url = mediaSourceUrl(part.source)
                return (
                  <img
                    key={index}
                    className="bubble-img lightboxable"
                    src={url}
                    alt=""
                    onClick={() => openLightbox(url)}
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
                    <IconFile size={12} strokeWidth={1.8} /> {part.filename}
                  </button>
                ) : (
                  <span key={index} className="bubble-file">
                    <IconFile size={12} strokeWidth={1.8} /> {part.filename}
                  </span>
                )
              }
              if (part.type === 'file_ref') {
                // @ 引用文件的紧凑芯片：仅显示主干名，完整路径与截断状态放 title
                const name = part.path.split('/').pop() ?? part.path
                const title = `${part.path}${part.truncated ? '（内容过长已截断）' : ''}`
                return onPreviewFile ? (
                  <button
                    key={index}
                    type="button"
                    className="bubble-file clickable"
                    onClick={() => onPreviewFile(part.path)}
                    title={`${title}｜在右侧边栏预览文件`}
                  >
                    <IconFile size={12} strokeWidth={1.8} /> {name}
                  </button>
                ) : (
                  <span key={index} className="bubble-file" title={title}>
                    <IconFile size={12} strokeWidth={1.8} /> {name}
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
  snap: { state: string; draft: ContentPart[]; session: Session; background_tasks: Record<string, BackgroundTask>; shell_outputs: Record<string, ShellOutput> } | null
  pending: string | null
  onRevert?: (index: number, prompt: string) => void
  onFork?: (index: number) => void
  onPreviewFile?: PreviewFile
}) {
  const turns = snap?.session.turns
  const draft = snap?.draft
  const backgroundTasks = snap?.background_tasks
  const items = useMemo(() => turns ? buildItems(turns) : [], [turns])
  const trace = useMemo(() => taskTrace([
    ...(turns?.flatMap(turn => turn.messages.flatMap(message => message.parts)) ?? []),
    ...(draft ?? []),
  ], backgroundTasks ?? {}), [turns, draft, backgroundTasks])
  const tasks = { ...trace, shellOutputs: snap?.shell_outputs }
  if (!snap) {
    return (
      <div className="chat-empty">
        <div className="empty-brand">vva</div>
        <div className="empty-hint">点击左侧「新对话」开始</div>
      </div>
    )
  }

  const running = snap.state !== ''
  const workspace = snap.session.workspace_root
  const showDraft = snap.draft.length > 0 || running
  // 运行中最后一个工作段即当前响应段：容器保持展开，结束后随 live 翻转自动收起
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
              tasks={tasks}
              live={index === liveWork}
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
            <Parts parts={snap.draft} live={running} workspace={workspace} onPreviewFile={onPreviewFile} tasks={tasks} />
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
