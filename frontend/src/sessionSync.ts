import type { Session, SessionDelta, SessionSnapshot, SessionSummary } from './types'

/** 当前与后台会话共用列表投影，完整保留服务端的可空字段。 */
export function sessionSummary(session: Session, state: string): SessionSummary {
  return {
    id: session.id,
    title: session.title,
    workspace_root: session.workspace_root,
    created_at: session.created_at,
    turn_count: session.turns.length,
    current_model: session.current_model,
    current_variant: session.current_variant,
    web_search: session.web_search,
    parent: session.parent,
    agent: session.agent,
    state,
  }
}

/** 每个连接独立维护版本基线，原子应用一帧中的历史、草稿和状态。 */
export function applySessionDelta(previous: SessionSnapshot, delta: SessionDelta): SessionSnapshot {
  if (delta.base_revision !== previous.revision || delta.revision <= previous.revision) {
    throw new Error('会话增量版本不匹配，请重新打开会话')
  }
  let session = previous.session
  if (delta.session && delta.session.id !== session.id) {
    throw new Error('会话增量 ID 不匹配')
  }
  if (delta.session || delta.turns) {
    let turns = session.turns
    if (delta.turns) {
      const { from, items } = delta.turns
      if (!Number.isInteger(from) || from < 0 || from > turns.length) {
        throw new Error('会话轮次增量越界')
      }
      turns = [...turns.slice(0, from), ...items]
    }
    session = { ...(delta.session ?? session), turns }
  }
  let draft = previous.draft
  if (delta.draft) {
    const { len, updates } = delta.draft
    if (!Number.isInteger(len) || len < 0) throw new Error('会话草稿长度无效')
    draft = draft.slice(0, len)
    for (const update of updates) {
      const { index } = update
      if (!Number.isInteger(index) || index < 0 || index >= len || index > draft.length) {
        throw new Error('会话草稿增量越界')
      }
      const part = draft[index]
      if (update.type === 'set') {
        draft[index] = update.part
      } else if (update.type === 'text' && part?.type === 'text') {
        draft[index] = { ...part, text: part.text + update.delta }
      } else if (update.type === 'reasoning' && part?.type === 'reasoning') {
        draft[index] = { ...part, content: part.content + update.delta }
      } else {
        throw new Error('会话草稿增量类型不匹配')
      }
    }
    if (draft.length !== len) throw new Error('会话草稿增量不完整')
  }
  let shellOutputs = previous.shell_outputs
  if (delta.shell_outputs) {
    shellOutputs = { ...shellOutputs }
    for (const patch of delta.shell_outputs) {
      if (patch.type === 'remove') {
        delete shellOutputs[patch.id]
      } else if (patch.type === 'set') {
        shellOutputs[patch.id] = patch.output
      } else {
        const old = shellOutputs[patch.id]
        if (!old || old.revision !== patch.base_revision || patch.revision <= old.revision ||
          !Number.isInteger(patch.drop) || patch.drop < 0 || patch.drop > old.text.length ||
          patch.start < old.start) {
          throw new Error('命令输出增量版本或区间不匹配')
        }
        shellOutputs[patch.id] = {
          revision: patch.revision, start: patch.start,
          text: old.text.slice(patch.drop) + patch.text,
          total_bytes: patch.total_bytes, finished: patch.finished, error: patch.error,
        }
      }
    }
  }
  return {
    shell_outputs: shellOutputs,
    pending_inputs: delta.pending_inputs ?? previous.pending_inputs,
    revision: delta.revision,
    state: delta.state,
    persisted: delta.persisted,
    notices: delta.notices ?? previous.notices,
    background_tasks: delta.background_tasks ?? previous.background_tasks,
    external_web: delta.external_web ?? previous.external_web,
    session,
    draft,
    pending_user_question:
      delta.pending_user_question === undefined
        ? previous.pending_user_question
        : delta.pending_user_question,
  }
}

export function subscribeSession(
  id: string,
  onSnapshot: (snapshot: SessionSnapshot) => void,
  onError: (error: unknown) => void,
): () => void {
  const source = new EventSource(`/api/sessions/${id}/events`)
  let current: SessionSnapshot | null = null
  const receive = (event: MessageEvent, snapshot: boolean) => {
    try {
      if (snapshot) {
        const next = JSON.parse(event.data) as SessionSnapshot
        if (next.session.id !== id) throw new Error('会话快照 ID 不匹配')
        current = next
      } else {
        if (!current) throw new Error('会话增量缺少初始快照')
        current = applySessionDelta(current, JSON.parse(event.data) as SessionDelta)
      }
      onSnapshot(current)
    } catch (error) {
      source.close()
      onError(error)
    }
  }
  source.addEventListener('snapshot', event => receive(event, true))
  source.addEventListener('delta', event => receive(event, false))
  // 网络中断由 EventSource 重连；新连接首帧重新建立版本基线。
  return () => source.close()
}
