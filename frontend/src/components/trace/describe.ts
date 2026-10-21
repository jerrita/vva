import type { ToolCall, ToolResult, WebSearchCall, WebSearchSource } from '../../types'
import type { FileDiff } from '../../utils/editDiff'
import {
  APPLY_PATCH,
  EDIT,
  GLOB,
  HASHLINE_EDIT,
  HASHLINE_GREP,
  HASHLINE_READ,
  GREP,
  READ,
  SHELL,
  WRITE,
} from '../../toolNames'

/**
 * 轨迹行的动作类别：决定轨道上的字形形状。
 * observe 空心圆（只读）/ change 实心紫圆（改动文件）/ execute 方块（执行命令）/ delegate 紫描边圆（交给他人：子代理、用户）
 */
export type StepClass = 'observe' | 'change' | 'execute' | 'delegate'

/** 运行段摘要的计数分类 */
export type TallyKey = '查看' | '搜索' | '修改' | '执行' | '委派'

export type Outcome = { text: string } | { plus: number; minus: number }

/** 单个工具调用的行模型：动词列 + 目标 + 右侧结果 */
export interface ToolView {
  verb: string
  cls: StepClass
  tally: TallyKey
  target: string
  /** 目标按代码/路径排版 */
  mono: boolean
  /** 目标后的弱化补充（Shell 命令、Grep 范围） */
  secondary?: string
  /** 原始路径：预览跳转用 */
  filePath?: string
  /** Read 的行区间：预览时高亮 */
  range?: { start: number; end: number }
  outcome?: Outcome
  /** 展开区内容：输出原文 / 编辑差异 / 无（仅预览或无内容） */
  body: 'output' | 'diff' | null
  /** Shell 展开区首行的命令 */
  command?: string
}

export function parseArgs(args: string): Record<string, unknown> | null {
  try {
    const value: unknown = JSON.parse(args)
    return value && typeof value === 'object' && !Array.isArray(value)
      ? (value as Record<string, unknown>)
      : null
  } catch {
    return null
  }
}

function str(value: unknown): string | null {
  return typeof value === 'string' ? value : null
}

/** 模型偶尔把路径整体包一层引号，展示与预览前剥掉 */
export function cleanPath(path: string): string {
  const trimmed = path.trim()
  return trimmed.length >= 2 && trimmed.startsWith('"') && trimmed.endsWith('"')
    ? trimmed.slice(1, -1)
    : trimmed
}

/** 项目内文件的展示路径相对化；项目外保持原样 */
export function relativePath(path: string, workspace?: string): string {
  if (!workspace) return path
  const root = workspace.replace(/\/+$/, '')
  if (path === root) return '.'
  if (path.startsWith(`${root}/`)) return path.slice(root.length + 1)
  return path
}

export function baseName(path: string): string {
  const filePath = path.split('#', 1)[0].replace(/\/+$/, '')
  return filePath.slice(filePath.lastIndexOf('/') + 1) || filePath
}

function nonEmptyLines(text: string): number {
  return text.split('\n').filter(line => line.trim() !== '' && line !== '[results truncated]').length
}

function diffStat(diffs: FileDiff[]): { plus: number; minus: number } | undefined {
  if (diffs.length === 0) return undefined
  return {
    plus: diffs.reduce((sum, diff) => sum + diff.plus, 0),
    minus: diffs.reduce((sum, diff) => sum + diff.minus, 0),
  }
}

/** 补丁类编辑的目标与补充：单文件显示路径（新建 / 删除 / `old → new` 作补充），多文件显示文件数 */
function describePatch(
  diffs: FileDiff[],
  rel: (path: string) => string,
): Pick<ToolView, 'target' | 'secondary' | 'filePath'> {
  if (diffs.length === 0) return { target: '（解析中…）' }
  if (diffs.length > 1) {
    const first = diffs.find(diff => diff.op !== 'delete') ?? diffs[0]
    return { target: `${diffs.length} files`, filePath: first.op === 'delete' ? undefined : first.path }
  }
  const [diff] = diffs
  const note =
    diff.op === 'add' ? '新建' : diff.op === 'delete' ? '删除' : diff.op === 'move' && diff.to ? `→ ${rel(diff.to)}` : undefined
  return {
    target: rel(diff.path),
    secondary: note,
    filePath: diff.op === 'delete' ? undefined : diff.to ?? diff.path,
  }
}

function host(url: string): string {
  try {
    const parsed = new URL(url)
    return parsed.host + (parsed.pathname === '/' ? '' : parsed.pathname)
  } catch {
    return url
  }
}

function questionSummary(args: Record<string, unknown>): string {
  const questions = Array.isArray(args.questions) ? args.questions : []
  const first = questions[0]
  if (!first || typeof first !== 'object') return '向你提问'
  const item = first as Record<string, unknown>
  const text = str(item.question) ?? str(item.header) ?? '向你提问'
  return questions.length > 1 ? `${text}（共 ${questions.length} 问）` : text
}

/**
 * 按工具名生成行模型；`diffs` 由调用方注入（编辑差异解析在独立模块，仅编辑类工具需要）。
 */
export function describeTool(
  call: ToolCall,
  result: ToolResult | null,
  workspace: string | undefined,
  diffs: FileDiff[],
): ToolView {
  const args = parseArgs(call.arguments) ?? {}
  const rel = (path: string) => relativePath(cleanPath(path), workspace)
  const ok = result != null && !result.is_error
  const fallback: ToolView = {
    verb: call.name,
    cls: 'execute',
    tally: '执行',
    target: call.arguments.length > 120 ? `${call.arguments.slice(0, 120)}…` : call.arguments,
    mono: true,
    body: 'output',
  }

  switch (call.name) {
    case READ:
    case HASHLINE_READ: {
      const path = str(args.path)
      if (!path) return { ...fallback, cls: 'observe', tally: '查看' }
      const offset = typeof args.offset === 'number' ? args.offset : null
      const limit = typeof args.limit === 'number' ? args.limit : null
      let outcome: Outcome | undefined
      let range: { start: number; end: number } | undefined
      if (limit != null) {
        const start = offset ?? 1
        range = { start, end: start + limit - 1 }
        outcome = { text: `L${start}–${range.end}` }
      } else if (offset != null && offset > 1) {
        outcome = { text: `L${offset}–` }
      }
      return {
        verb: 'Read',
        cls: 'observe',
        tally: '查看',
        target: rel(path),
        mono: true,
        filePath: cleanPath(path),
        range,
        outcome,
        body: null,
      }
    }
    case WRITE: {
      const path = str(args.path)
      const content = str(args.content)
      if (!path) return { ...fallback, cls: 'change', tally: '修改' }
      return {
        verb: 'Write',
        cls: 'change',
        tally: '修改',
        target: rel(path),
        mono: true,
        filePath: cleanPath(path),
        outcome: content != null ? { plus: content === '' ? 0 : content.split('\n').length, minus: 0 } : undefined,
        body: 'diff',
      }
    }
    case EDIT: {
      const path = str(args.path)
      return {
        verb: 'Edit',
        cls: 'change',
        tally: '修改',
        target: path ? rel(path) : '（未识别的编辑）',
        mono: true,
        filePath: path ? cleanPath(path) : undefined,
        outcome: diffStat(diffs),
        body: 'diff',
      }
    }
    case APPLY_PATCH:
    case HASHLINE_EDIT:
      return {
        ...describePatch(diffs, rel),
        verb: call.name === APPLY_PATCH ? 'Patch' : 'Edit',
        cls: 'change',
        tally: '修改',
        mono: true,
        outcome: diffStat(diffs),
        body: 'diff',
      }
    case GLOB: {
      const pattern = str(args.pattern) ?? call.arguments
      const path = str(args.path)
      const scope = path ? rel(path) : null
      return {
        verb: 'Glob',
        cls: 'observe',
        tally: '查看',
        target: pattern,
        mono: true,
        secondary: scope && scope !== '.' ? scope : undefined,
        outcome: ok ? { text: `${nonEmptyLines(result.content)} 个` } : undefined,
        body: 'output',
      }
    }
    case GREP:
    case HASHLINE_GREP: {
      const pattern = str(args.pattern) ?? call.arguments
      const include = str(args.include)
      const path = str(args.path)
      const scope = path ? rel(path) : null
      const where = [scope && scope !== '.' ? scope : null, include].filter(Boolean).join(' ')
      return {
        verb: 'Grep',
        cls: 'observe',
        tally: '查看',
        target: pattern,
        mono: true,
        secondary: where || undefined,
        outcome: ok ? { text: `${nonEmptyLines(result.content)} 处` } : undefined,
        body: 'output',
      }
    }
    case SHELL: {
      const command = str(args.command) ?? ''
      const intent = str(args.intent)
      const background = args.run_in_background === true
      const outputLines = ok ? nonEmptyLines(result.content) : 0
      return {
        verb: 'Shell',
        cls: 'execute',
        tally: '执行',
        target: intent || command,
        mono: !intent,
        secondary: intent ? command : undefined,
        outcome: background
          ? { text: '后台' }
          : outputLines > 0
            ? { text: `${outputLines} 行` }
            : undefined,
        body: 'output',
        command,
      }
    }
    case 'TaskOutput':
    case 'TaskStop':
      return {
        verb: call.name === 'TaskOutput' ? 'Task' : 'Stop',
        cls: 'execute',
        tally: '执行',
        target: str(args.task_id) ?? call.arguments,
        mono: true,
        body: 'output',
      }
    case 'Agent': {
      const agent = str(args.agent) ?? 'agent'
      const description = str(args.description)
      return {
        verb: 'Agent',
        cls: 'delegate',
        tally: '委派',
        target: description ?? agent,
        mono: false,
        outcome: { text: args.run_in_background === true ? `${agent} · 后台` : agent },
        body: 'output',
      }
    }
    case 'SendMessage': {
      const text = str(args.text) ?? ''
      return {
        verb: 'Send',
        cls: 'delegate',
        tally: '委派',
        target: text,
        mono: false,
        outcome: { text: str(args.session_id) ?? '' },
        body: 'output',
      }
    }
    case 'AskUser':
      return {
        verb: 'Ask',
        cls: 'delegate',
        tally: '委派',
        target: questionSummary(args),
        mono: false,
        body: 'output',
      }
    case 'Skill':
      return {
        verb: 'Skill',
        cls: 'observe',
        tally: '查看',
        target: str(args.name) ?? call.arguments,
        mono: true,
        body: null,
      }
    default:
      return fallback
  }
}

/**
 * 外置 web_search 工具输出中的编号条目 → sources。
 * 与工具输出格式耦合：`N. title` 行后紧跟 URL 行（title 缺省时该行为 `N. url`）。
 */
function parseWebSources(content: string): WebSearchSource[] {
  const lines = content.split('\n')
  const sources: WebSearchSource[] = []
  for (let index = 0; index < lines.length; index++) {
    const entry = /^\d+\. (.+)$/.exec(lines[index])
    if (!entry) continue
    const candidate = (lines[index + 1] ?? '').trim()
    if (/^https?:\/\//.test(candidate)) {
      sources.push({ url: candidate, title: entry[1] === candidate ? '' : entry[1] })
      index++
    } else if (/^https?:\/\//.test(entry[1])) {
      sources.push({ url: entry[1], title: entry[1] })
    }
  }
  return sources
}

/**
 * 外置 web 工具（web_search / web_fetch）→ 原生 WebSearchCall：
 * 与服务端原生搜索共用同一行形态，其余工具返回 null。
 */
export function externalWebCall(call: ToolCall, result: ToolResult | null): WebSearchCall | null {
  const args = parseArgs(call.arguments)
  const status = !result ? 'searching' : result.is_error ? 'failed' : 'completed'
  if (call.name === 'web_search') {
    return {
      id: call.id,
      query: str(args?.query) ?? '',
      action: { type: 'search' },
      status,
      sources: result && !result.is_error ? parseWebSources(result.content) : [],
    }
  }
  if (call.name === 'web_fetch') {
    const url = str(args?.url)
    if (!url) return null
    return { id: call.id, query: url, action: { type: 'open_page', url }, status, sources: [] }
  }
  return null
}

/** 原生/外置搜索调用的行模型 */
export function describeSearch(call: WebSearchCall): { verb: string; target: string; outcome?: string } {
  const action = call.action
  const sources = call.sources?.length ?? 0
  if (action?.type === 'open_page') return { verb: 'Open', target: host(action.url) }
  if (action?.type === 'find_in_page') {
    return { verb: 'Find', target: `“${action.pattern}” · ${host(action.url)}` }
  }
  return {
    verb: 'Search',
    target: call.query || '（查询未下发）',
    outcome: sources > 0 ? `${sources} 来源` : undefined,
  }
}
