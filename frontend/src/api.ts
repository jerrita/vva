import type { ModelsResponse, SessionSnapshot, SessionSummary, SidebarStats } from './types'

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
  sessions: () => request<SessionSummary[]>('/api/sessions'),
  createSession: (body: { model?: string; variant?: string | null; search?: boolean } = {}) =>
    request<SessionSnapshot>('/api/sessions', json('POST', body)),
  deleteSession: (id: string) => request<void>(`/api/sessions/${id}`, { method: 'DELETE' }),
  submit: (id: string, prompt: string) =>
    request<void>(`/api/sessions/${id}/messages`, json('POST', { prompt })),
  abort: (id: string) => request<void>(`/api/sessions/${id}/abort`, { method: 'POST' }),
  configure: (id: string, body: ConfigureBody) =>
    request<SessionSnapshot>(`/api/sessions/${id}/config`, json('POST', body)),
  stats: (id: string) => request<SidebarStats>(`/api/sessions/${id}/stats`),
}
