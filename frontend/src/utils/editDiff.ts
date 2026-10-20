/**
 * 文件修改类工具调用（Edit / Write 的 JSON 入参，apply_patch / hashline_edit 的补丁原文）→ 结构化 diff。
 * 纯函数、无 React 依赖；按工具名精确分支，入参可能流式不完整，解析容忍截断；识别不了时返回 []。
 */
import { APPLY_PATCH, EDIT, HASHLINE_EDIT, WRITE } from '../toolNames'

export type DiffLine = { kind: 'add' | 'del' | 'ctx' | 'hunk'; text: string }

export interface FileDiff {
  path: string
  lines: DiffLine[]
  plus: number
  minus: number
  op?: 'add' | 'update' | 'delete' | 'move'
  /** op 为 move 时的目标路径 */
  to?: string
}

/** 单个文件最多输出的 diff 行数，超出部分折叠为一条 hunk 提示。 */
const MAX_LINES = 400
/** replace 模式下，old/new 公共首尾保留的上下文行数上限。 */
const MAX_CTX = 2

/** 剥离模型输出路径外层包裹的一层双引号。 */
function cleanPath(raw: string): string {
  const p = raw.trim()
  return p.length >= 2 && p.startsWith('"') && p.endsWith('"') ? p.slice(1, -1) : p
}

/** 文本按行切分；末尾换行不产生多余空行，空串视为零行。 */
function splitLines(text: string): string[] {
  if (text === '') return []
  const lines = text.replace(/\r\n/g, '\n').split('\n')
  if (lines[lines.length - 1] === '') lines.pop()
  return lines
}

/** 从行列表构造 FileDiff：统计 plus/minus 并做截断（统计基于截断前）。 */
function build(
  path: string,
  lines: DiffLine[],
  op?: FileDiff['op'],
  extra?: { plus?: number; minus?: number },
): FileDiff {
  let plus = 0
  let minus = 0
  for (const l of lines) {
    if (l.kind === 'add') plus++
    else if (l.kind === 'del') minus++
  }
  let out = lines
  if (lines.length > MAX_LINES) {
    out = lines.slice(0, MAX_LINES)
    out.push({ kind: 'hunk', text: `… ${lines.length - MAX_LINES} more lines` })
  }
  const diff: FileDiff = {
    path,
    lines: out,
    plus: plus + (extra?.plus ?? 0),
    minus: minus + (extra?.minus ?? 0),
  }
  if (op) diff.op = op
  return diff
}

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v)
}

// ---------------------------------------------------------------- replace

/** 单个 old→new 替换：公共首尾折叠为至多 MAX_CTX 行 ctx，中间为 del/add。 */
function replaceLines(oldStr: string, newStr: string): DiffLine[] {
  const a = splitLines(oldStr)
  const b = splitLines(newStr)
  let head = 0
  while (head < a.length && head < b.length && a[head] === b[head]) head++
  let tail = 0
  while (
    tail < a.length - head &&
    tail < b.length - head &&
    a[a.length - 1 - tail] === b[b.length - 1 - tail]
  ) {
    tail++
  }
  const out: DiffLine[] = []
  for (const t of a.slice(Math.max(0, head - MAX_CTX), head)) out.push({ kind: 'ctx', text: t })
  for (const t of a.slice(head, a.length - tail)) out.push({ kind: 'del', text: t })
  for (const t of b.slice(head, b.length - tail)) out.push({ kind: 'add', text: t })
  const tailCtx = a.slice(a.length - tail, a.length - tail + Math.min(tail, MAX_CTX))
  for (const t of tailCtx) out.push({ kind: 'ctx', text: t })
  return out
}

function replaceDiff(args: Record<string, unknown>): FileDiff[] {
  if (typeof args.path !== 'string') return []
  const pairs: Array<[string, string]> = []
  if (Array.isArray(args.edits)) {
    for (const e of args.edits) {
      if (isRecord(e) && typeof e.old_string === 'string' && typeof e.new_string === 'string') {
        pairs.push([e.old_string, e.new_string])
      }
    }
  } else if (typeof args.old_string === 'string' && typeof args.new_string === 'string') {
    pairs.push([args.old_string, args.new_string])
  }
  if (pairs.length === 0) return []
  const lines: DiffLine[] = []
  pairs.forEach(([o, n], i) => {
    if (pairs.length > 1) lines.push({ kind: 'hunk', text: `@@ edit ${i + 1}/${pairs.length}` })
    lines.push(...replaceLines(o, n))
  })
  return [build(cleanPath(args.path), lines, 'update')]
}

// ---------------------------------------------------------------- patch (V4A)

/** 对应后端 apply_patch.rs 语法：Add/Delete/Update File、Move to、@@ 块、End of File 标记。 */
function patchDiffs(input: string): FileDiff[] {
  let rows = input.trim().split('\n').map((r) => r.replace(/\r$/, ''))
  // 可选 heredoc 包装：<<EOF ... EOF
  if (
    rows.length >= 4 &&
    ['<<EOF', "<<'EOF'", '<<"EOF"'].includes(rows[0].trim()) &&
    rows[rows.length - 1].endsWith('EOF')
  ) {
    rows = rows.slice(1, -1)
  }
  if (rows[0] !== '*** Begin Patch') return []

  type Cur = {
    path: string
    op: NonNullable<FileDiff['op']>
    lines: DiffLine[]
    chunk: boolean
    to?: string
  }
  const files: FileDiff[] = []
  let cur: Cur | null = null
  const flush = () => {
    if (cur) {
      const diff = build(cur.path, cur.lines, cur.op)
      if (cur.to) diff.to = cur.to
      files.push(diff)
    }
    cur = null
  }
  const start = (path: string, op: Cur['op']) => {
    flush()
    cur = { path: cleanPath(path), op, lines: [], chunk: false }
    if (op === 'delete') cur.lines.push({ kind: 'hunk', text: '删除整个文件' })
  }

  for (const row of rows.slice(1)) {
    if (row === '*** End Patch') break
    if (row.startsWith('*** Add File: ')) start(row.slice(13), 'add')
    else if (row.startsWith('*** Delete File: ')) start(row.slice(16), 'delete')
    else if (row.startsWith('*** Update File: ')) start(row.slice(17), 'update')
    else if (!cur) continue // 首个文件操作之前的前导（如 Environment ID）
    else {
      const c: Cur = cur
      if (c.op === 'add') {
        if (row.startsWith('+')) c.lines.push({ kind: 'add', text: row.slice(1) })
      } else if (c.op === 'delete') {
        continue
      } else {
        const trimmed = row.trimEnd()
        if (trimmed === '@@' || trimmed.startsWith('@@ ')) {
          c.chunk = true
          c.lines.push({ kind: 'hunk', text: trimmed })
        } else if (trimmed === '*** End of File') {
          // 语法保留标记，无语义
        } else if (trimmed === '') {
          if (c.chunk) c.lines.push({ kind: 'ctx', text: '' })
        } else if (row.startsWith('+')) {
          if (c.chunk) c.lines.push({ kind: 'add', text: row.slice(1) })
        } else if (row.startsWith('-')) {
          if (c.chunk) c.lines.push({ kind: 'del', text: row.slice(1) })
        } else if (row.startsWith(' ')) {
          if (c.chunk) c.lines.push({ kind: 'ctx', text: row.slice(1) })
        } else if (trimmed.startsWith('*** Move to: ') && !c.chunk) {
          c.op = 'move'
          c.to = cleanPath(trimmed.slice(13))
        }
      }
    }
  }
  flush()
  return files
}

// ---------------------------------------------------------------- hashline

/** `[PATH#TAG]` 编辑既有文件；`[PATH]`（无 TAG）配合 ADD 新建 */
const HEADER_RE = /^\[(.+?)(?:#([0-9A-Za-z]+))?\]$/

/**
 * 从 PUT/CUT 定位符估算被删除行数（未知返回 0）：
 *   N.=M  → M-N+1；单行 N（仅 CUT）→ 1；
 *   N*（块，需读文件才知边界）、<N、>N、>$、>N*（纯插入）→ 0。
 */
function rangeSize(locator: string): number {
  const m = /^(\d+)\.=(\d+)$/.exec(locator)
  if (m) return Math.max(0, Number(m[2]) - Number(m[1]) + 1)
  return 0
}

/**
 * 对应后端 hashline/parse.rs：
 *   [PATH#TAG]        开启新文件段（FileDiff，默认 op 'update'）；[PATH] 配合 ADD
 *   ADD               新建文件：op 'add'，后续 + 行为新增正文
 *   PUT N.=M:         hunk 行原样；+ 行 → add；minus += M-N+1（替换区间）
 *   PUT N*: / <N: / >N: / >$: / >N*:
 *                     hunk 行原样；+ 行 → add；块替换边界未知不计 minus，插入无删除
 *   PUT ... @reg      寄存器粘贴：hunk 行原样；无正文；N.=M 粘贴同样替换区间 → minus += M-N+1
 *   CUT N / N.=M      hunk 行原样；minus += 区间行数（单行为 1）
 *   CUT N*            hunk 行原样；块边界未知，不计 minus
 *   REM               整文件删除：op 'delete'，一行 hunk 说明
 *   MV dest           整文件移动：op 'move'（若已是 delete 则保持），目标记入 to
 *   Begin/End Patch   可选外壳，忽略；空行、无法识别的行忽略
 */
function hashlineDiffs(input: string): FileDiff[] {
  const rows = input.split('\n').map((r) => r.replace(/\r$/, ''))
  const files: FileDiff[] = []
  let cur: { path: string; lines: DiffLine[]; op: FileDiff['op']; minus: number; to?: string } | null = null
  let inBody = false
  const flush = () => {
    if (cur) {
      const diff = build(cur.path, cur.lines, cur.op, { minus: cur.minus })
      if (cur.to) diff.to = cur.to
      files.push(diff)
    }
    cur = null
    inBody = false
  }
  for (const row of rows) {
    const t = row.trimEnd()
    const header = HEADER_RE.exec(t)
    if (header) {
      flush()
      cur = { path: cleanPath(header[1]), lines: [], op: 'update', minus: 0 }
      continue
    }
    if (!cur) continue
    if (t.startsWith('+') && inBody) {
      cur.lines.push({ kind: 'add', text: row.slice(1) })
      continue
    }
    const s = t.trim()
    if (s === '') continue
    let m: RegExpExecArray | null
    if (/^ADD:?$/.test(s)) {
      cur.op = 'add'
      inBody = true
    } else if (/^REM$/.test(s)) {
      cur.op = 'delete'
      cur.lines.push({ kind: 'hunk', text: '删除整个文件' })
      inBody = false
    } else if ((m = /^MV\s+(\S.*)$/.exec(s))) {
      if (cur.op !== 'delete') cur.op = 'move'
      cur.to = cleanPath(m[1])
      inBody = false
    } else if ((m = /^(PUT|CUT)\s*(\S+?)\s*(@[\w-]+)?\s*(:)?$/.exec(s))) {
      const [, kw, locator, reg, colon] = m
      const size = rangeSize(locator)
      if (kw === 'CUT') cur.minus += size || (/^\d+$/.test(locator) ? 1 : 0)
      else cur.minus += size
      cur.lines.push({ kind: 'hunk', text: s })
      inBody = kw === 'PUT' && !!colon && !reg
    }
    // 其余行（Begin/End Patch 外壳等）忽略
  }
  flush()
  return files
}

// ---------------------------------------------------------------- write

function writeDiff(args: Record<string, unknown>): FileDiff[] {
  if (typeof args.path !== 'string' || typeof args.content !== 'string') return []
  const lines = splitLines(args.content).map((text): DiffLine => ({ kind: 'add', text }))
  return [build(cleanPath(args.path), lines, 'add')]
}

// ---------------------------------------------------------------- hashline 结果

/**
 * 解析 hashline_edit 的执行结果：每段为 `[PATH#TAG]` 头 + 统一 diff（`@@ -a,b +c,d @@` 带行号）+
 * 可选的 "Current snapshot preview" 预览（忽略）；删除段为 `Deleted file PATH`。
 */
function hashlineResultDiffs(content: string): Map<string, FileDiff> {
  const out = new Map<string, FileDiff>()
  let cur: { path: string; lines: DiffLine[] } | null = null
  let inHunk = false
  const flush = () => {
    if (cur) out.set(cur.path, build(cur.path, cur.lines, 'update'))
    cur = null
    inHunk = false
  }
  for (const row of content.replace(/\r\n/g, '\n').split('\n')) {
    const header = HEADER_RE.exec(row.trimEnd())
    if (header && header[2] !== undefined) {
      flush()
      cur = { path: cleanPath(header[1]), lines: [] }
      continue
    }
    if (!cur) continue
    if (row.startsWith('Current snapshot preview')) {
      flush()
      continue
    }
    if (row.startsWith('@@')) {
      inHunk = true
      cur.lines.push({ kind: 'hunk', text: row.trimEnd() })
    } else if (!inHunk) {
      continue // `--- path` / `+++ path` 文件头
    } else if (row.startsWith('+')) cur.lines.push({ kind: 'add', text: row.slice(1) })
    else if (row.startsWith('-')) cur.lines.push({ kind: 'del', text: row.slice(1) })
    else if (row.startsWith(' ')) cur.lines.push({ kind: 'ctx', text: row.slice(1) })
  }
  flush()
  return out
}

/** 成功结果中的真实 diff 取代按输入推算的行；ADD / REM 等无结果 diff 的段保持输入形态。 */
function mergeHashlineResult(inputs: FileDiff[], content: string): FileDiff[] {
  const real = hashlineResultDiffs(content)
  return inputs.map((d) => {
    const hit = d.op === 'update' || d.op === 'move' ? real.get(d.path) : undefined
    return hit ? { ...d, lines: hit.lines, plus: hit.plus, minus: hit.minus } : d
  })
}

function parseJson(raw: string): Record<string, unknown> | null {
  try {
    const v: unknown = JSON.parse(raw)
    return isRecord(v) ? v : null
  } catch {
    return null
  }
}

/**
 * 按工具名解析编辑 diff；识别不了返回 []。
 * `result` 为 hashline_edit 成功时的结果原文，用其真实 diff 替换推算行。
 */
export function editDiffs(toolName: string, rawArgs: string, result?: string): FileDiff[] {
  switch (toolName) {
    case WRITE: {
      const args = parseJson(rawArgs)
      return args ? writeDiff(args) : []
    }
    case EDIT: {
      const args = parseJson(rawArgs)
      return args ? replaceDiff(args) : []
    }
    case APPLY_PATCH:
      return patchDiffs(rawArgs)
    case HASHLINE_EDIT: {
      const inputs = hashlineDiffs(rawArgs)
      return result ? mergeHashlineResult(inputs, result) : inputs
    }
    default:
      return []
  }
}
