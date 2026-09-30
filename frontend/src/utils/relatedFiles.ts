import type { ContentPart, RelatedFile, Session } from '../types'
import { cleanPath, parseArgs, relativePath } from '../components/trace/describe'
import { editDiffs, type FileDiff } from './editDiff'

type Action = RelatedFile['actions'][number]

/** 一次成功（或尚无结果）的工具调用 / 附件，按时间顺序产出。 */
type Touch =
  | { kind: 'call'; name: string; arguments: string; turnIndex: number }
  | { kind: 'file'; filename: string; turnIndex: number }

/** 按序遍历会话与草稿，过滤掉 tool_result 报错的调用。 */
function collectTouches(session: Session, draft?: ContentPart[]): Touch[] {
  const groups: { parts: ContentPart[]; turnIndex: number }[] = []
  session.turns.forEach((turn, turnIndex) => {
    for (const msg of turn.messages) groups.push({ parts: msg.parts, turnIndex })
  })
  if (draft && draft.length > 0) groups.push({ parts: draft, turnIndex: session.turns.length })

  const failed = new Set<string>()
  for (const g of groups) {
    for (const part of g.parts) {
      if (part.type === 'tool_result' && part.is_error) failed.add(part.tool_call_id)
    }
  }

  const out: Touch[] = []
  for (const g of groups) {
    for (const part of g.parts) {
      if (part.type === 'tool_call') {
        if (failed.has(part.id)) continue
        out.push({ kind: 'call', name: part.name, arguments: part.arguments, turnIndex: g.turnIndex })
      } else if (part.type === 'file' && part.filename) {
        out.push({ kind: 'file', filename: part.filename, turnIndex: g.turnIndex })
      }
    }
  }
  return out
}

function normalize(path: string, workspace: string): string {
  return relativePath(cleanPath(path), workspace)
}

/** 本会话中所有成功的 Edit / Write diff，按调用顺序，仅保留规范化相对路径等于 rel 的。 */
export function sessionDiffsFor(
  session: Session,
  draft: ContentPart[] | undefined,
  rel: string,
): FileDiff[] {
  const out: FileDiff[] = []
  for (const t of collectTouches(session, draft)) {
    if (t.kind !== 'call' || (t.name !== 'Edit' && t.name !== 'Write')) continue
    for (const d of editDiffs(t.name, t.arguments)) {
      if (normalize(d.path, session.workspace_root) === rel) out.push(d)
    }
  }
  return out
}

function positiveInt(v: unknown): number | null {
  return typeof v === 'number' && Number.isFinite(v) && v >= 1 ? Math.floor(v) : null
}

/**
 * 从会话轮次与当前草稿中提取涉及的文件（Read / Write / Edit / 附件）。
 * 排序：有改动的文件在前，各自按最近触达倒序。
 */
export function extractRelatedFiles(
  session: Session | null,
  draft?: ContentPart[],
): RelatedFile[] {
  if (!session) return []
  const ws = session.workspace_root
  const map = new Map<string, RelatedFile>()
  let seq = 0

  const touch = (raw: string, action: Action, turnIndex: number): RelatedFile | null => {
    const path = cleanPath(raw)
    if (!path) return null
    const rel = relativePath(path, ws)
    let f = map.get(rel)
    if (!f) {
      const parts = rel.split('/').filter(Boolean)
      const name = parts.pop() || rel
      f = {
        path,
        rel,
        name,
        dir: parts.join('/'),
        actions: [],
        plus: 0,
        minus: 0,
        deleted: false,
        readRanges: [],
        turnIndex,
        lastIndex: 0,
      }
      map.set(rel, f)
    }
    f.path = path
    if (!f.actions.includes(action)) f.actions.push(action)
    f.turnIndex = Math.max(f.turnIndex, turnIndex)
    f.lastIndex = ++seq
    return f
  }

  for (const t of collectTouches(session, draft)) {
    if (t.kind === 'file') {
      touch(t.filename, 'attachment', t.turnIndex)
    } else if (t.name === 'Read') {
      const args = parseArgs(t.arguments)
      if (!args || typeof args.path !== 'string') continue
      const f = touch(args.path, 'read', t.turnIndex)
      if (!f) continue
      const limit = positiveInt(args.limit)
      if (limit !== null) {
        const start = positiveInt(args.offset) ?? 1
        f.readRanges.push({ start, end: start + limit - 1 })
      }
    } else if (t.name === 'Edit' || t.name === 'Write') {
      const action: Action = t.name === 'Edit' ? 'edit' : 'write'
      for (const d of editDiffs(t.name, t.arguments)) {
        const f = touch(d.path, action, t.turnIndex)
        if (!f) continue
        f.plus += d.plus
        f.minus += d.minus
        if (d.op === 'delete') f.deleted = true
        else if (d.op === 'add') f.deleted = false
      }
    }
  }

  const changed = (f: RelatedFile) => f.actions.includes('write') || f.actions.includes('edit')
  return [...map.values()].sort(
    (a, b) => Number(changed(b)) - Number(changed(a)) || b.lastIndex - a.lastIndex,
  )
}
