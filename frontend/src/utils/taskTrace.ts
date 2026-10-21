import type { BackgroundTask, ContentPart, ToolCall, ToolResult } from '../types'

export interface TaskQuery {
  call: ToolCall
  result: ToolResult | null
}

export interface TaskTrace {
  tasks: Record<string, BackgroundTask>
  queryOwners: Map<string, string>
  queries: Map<string, TaskQuery[]>
}

function argument(call: ToolCall, key: string): string | null {
  try {
    const value = JSON.parse(call.arguments)[key]
    return typeof value === 'string' ? value : null
  } catch {
    return null
  }
}

/** 只归并有明确启动身份的输出查询。 */
export function taskTrace(parts: ContentPart[], tasks: Record<string, BackgroundTask>): TaskTrace {
  const results = new Map<string, ToolResult>()
  for (const part of parts) {
    if (part.type === 'tool_result') results.set(part.tool_call_id, part)
  }
  const owners = new Map<string, string>()
  const trace: TaskTrace = { tasks, queryOwners: new Map(), queries: new Map() }
  for (const part of parts) {
    if (part.type !== 'tool_call') continue
    const result = results.get(part.id)
    if (result?.background_task_id) owners.set(result.background_task_id, part.id)
    const id = part.name === 'TaskOutput' ? argument(part, 'task_id') : null
    const owner = id ? owners.get(id) : undefined
    if (!owner) continue
    trace.queryOwners.set(part.id, owner)
    const queries = trace.queries.get(owner) ?? []
    queries.push({ call: part, result: result ?? null })
    trace.queries.set(owner, queries)
  }
  return trace
}

export function taskLabel(task: BackgroundTask): string {
  switch (task.status) {
    case 'running': return '后台运行中'
    case 'completed': return '已完成'
    case 'failed': return task.exit_code == null ? '失败' : `退出码 ${task.exit_code}`
    case 'stopped': return '已停止'
    case 'interrupted': return '已中断'
  }
}
