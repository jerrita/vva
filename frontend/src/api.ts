import type { FileAttachment, ModelsResponse, SessionSnapshot, SessionSummary, SidebarStats, SkillsResponse, UserAnswer } from './types'

export function toError(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(path, init)
  if (!response.ok) {
    const body: { error?: string } | null = await response.json().catch(() => null)
    throw new Error(body?.error ?? `请求失败 (${response.status})`)
  }
  // 202/204 等无响应体的成功返回
  if (response.status === 204) return undefined as T
  const text = await response.text()
  return text ? (JSON.parse(text) as T) : (undefined as T)
}

function json(method: string, body: unknown): RequestInit {
  return {
    method,
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  }
}

export interface ConfigureBody {
  model?: string
  /** null 表示清空推理档位，省略表示保持不变 */
  variant?: string | null
  search?: boolean
}

export const api = {
  models: () => request<ModelsResponse>('/api/models'),
  skills: () => request<SkillsResponse>('/api/skills'),
  workspace: () => request<{ root: string }>('/api/workspace'),
  dirs: (path: string) =>
    request<{ root: string; parent: string | null; dirs: string[] }>(
      `/api/fs/dirs?path=${encodeURIComponent(path)}`,
    ),
  sessions: () => request<SessionSummary[]>('/api/sessions'),
  createSession: (body: {
    model?: string
    variant?: string | null
    search?: boolean
    workspace?: string
  } = {}) => request<SessionSnapshot>('/api/sessions', json('POST', body)),
  deleteSession: (id: string) => request<void>(`/api/sessions/${id}`, { method: 'DELETE' }),
  submit: (id: string, prompt: string, files: FileAttachment[] = []) =>
    request<void>(`/api/sessions/${id}/messages`, json('POST', { prompt, files })),
  uploadFile: (filename: string, data: string) =>
    request<{ path: string }>('/api/files', json('POST', { filename, data })),
  /** 回退到指定用户轮次：丢弃该轮次及其后的内容 */
  revert: (id: string, turnIndex: number) =>
    request<SessionSnapshot>(`/api/sessions/${id}/revert`, json('POST', { turn_index: turnIndex })),
  /** 从指定用户轮次分叉新会话：保留该轮次之前的历史 */
  fork: (id: string, turnIndex: number) =>
    request<SessionSnapshot>(`/api/sessions/${id}/fork`, json('POST', { turn_index: turnIndex })),
  abort: (id: string) => request<void>(`/api/sessions/${id}/abort`, { method: 'POST' }),
  answerUser: (id: string, requestId: string, answers: UserAnswer[]) =>
    request<void>(`/api/sessions/${id}/answer`, json('POST', { request_id: requestId, answers })),
  configure: (id: string, body: ConfigureBody) =>
    request<SessionSnapshot>(`/api/sessions/${id}/config`, json('POST', body)),
  stats: (id: string) => request<SidebarStats>(`/api/sessions/${id}/stats`),
  /** 读取会话相关文件的实时内容，供前端侧栏预览 */
  sessionFile: (id: string, path: string) =>
    request<import('./types').SessionFileResponse>(
      `/api/sessions/${encodeURIComponent(id)}/file?path=${encodeURIComponent(path)}`,
    ),
  /** 列出工程目录下的条目（目录与文件），支持工程文件浏览器懒加载 */
  fsEntries: (workspace?: string, path?: string) => {
    const params = new URLSearchParams()
    if (workspace) params.set('workspace', workspace)
    if (path) params.set('path', path)
    const qs = params.toString()
    return request<import('./types').FsEntriesResponse>(`/api/fs/entries${qs ? `?${qs}` : ''}`)
  },
  /** 读取指定工作区文件内容 */
  fsFile: (path: string, workspace?: string) => {
    const params = new URLSearchParams({ path })
    if (workspace) params.set('workspace', workspace)
    return request<import('./types').SessionFileResponse>(`/api/fs/file?${params.toString()}`)
  },
  /** 全局配置文件原文（JSONC）；文件不存在时 content 为 null */
  getConfig: () => request<{ path: string; content: string | null }>('/api/config'),
  /** 保存全局配置：后端校验解析通过后落盘并热重载 */
  saveConfig: (content: string) => request<void>('/api/config', json('POST', { content })),
}
