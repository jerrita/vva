// 与 vva 领域模型的 serde 序列化格式一一对应

export interface TokenUsage {
  prompt_tokens: number
  completion_tokens: number
  total_tokens: number
  cached_tokens?: number
  reasoning_tokens?: number
}

export type Role = 'system' | 'user' | 'assistant' | 'tool'

export interface UrlCitation {
  type: 'url_citation'
  url: string
  title: string
  start_index: number
  end_index: number
}

export interface FileCitation {
  type: 'file_citation'
  file_id: string
  filename: string
  quote?: string
  start_index: number
  end_index: number
}

export interface WebSearchResultLocation {
  type: 'web_search_result_location'
  url: string
  title: string
  cited_text: string
}

export type Annotation = UrlCitation | FileCitation | WebSearchResultLocation

export type MediaSource =
  | { kind: 'url'; url: string }
  | { kind: 'base64'; data: string; mime_type: string }

export interface MediaContent {
  kind: 'image' | 'pdf' | 'audio' | 'video'
  mime_type: string
  filename: string
  data: string
}

export interface WebSearchSource {
  url: string
  title: string
  snippet?: string
}

export type WebSearchAction =
  | { type: 'search' }
  | { type: 'open_page'; url: string }
  | { type: 'find_in_page'; url: string; pattern: string }

export type SearchStatus = 'pending' | 'searching' | 'completed' | 'failed'

export interface WebSearchCall {
  id: string
  query: string
  action: WebSearchAction
  status: SearchStatus
  sources?: WebSearchSource[]
}

export interface ToolCall {
  id: string
  name: string
  arguments: string
}

export interface ToolResult {
  tool_call_id: string
  content: string
  attachments?: MediaContent[]
  is_error: boolean
}

export interface UserQuestionOption {
  label: string
  description: string
}

export interface UserQuestion {
  header: string
  question: string
  options: UserQuestionOption[]
  multi_select: boolean
}

export interface UserQuestionRequest {
  id: string
  questions: UserQuestion[]
}

export interface UserAnswer {
  question: string
  answers: string[]
}

/** 随消息提交的原生文件附件（Base64） */
export interface FileAttachment {
  filename: string
  mime_type: string
  data: string
}

export type ContentPart =
  | { type: 'text'; text: string; annotations?: Annotation[] }
  | { type: 'reasoning'; content: string; signature?: string }
  | { type: 'image'; source: MediaSource; mime_type?: string }
   | { type: 'file'; source: MediaSource; mime_type: string; filename: string }
  | ({ type: 'tool_call' } & ToolCall)
  | ({ type: 'tool_result' } & ToolResult)
  | ({ type: 'web_search_call' } & WebSearchCall)

export interface Message {
  parts: ContentPart[]
}

export interface Turn {
  id: string
  role: Role
  messages: Message[]
  usage?: TokenUsage
  elapsed_ms?: number
  worked_ms?: number
}

export interface Session {
  id: string
  title?: string | null
  workspace_root: string
  created_at: string
  updated_at: string
  current_model: string
  current_variant?: string | null
  web_search: boolean
  editor?: 'replace' | 'patch' | 'hashline'
  turns: Turn[]
}

export interface SessionSummary {
  id: string
  title?: string | null
  workspace_root: string
  created_at: string
  turn_count: number
  current_model: string
  current_variant?: string | null
  web_search: boolean
}

export interface SessionSnapshot {
  revision: number
  state: string
  persisted: boolean
  notices: string[]
  session: Session
  draft: ContentPart[]
  pending_user_question?: UserQuestionRequest | null
}

/** Web 传输增量；session 元信息整体替换，缺席的可选字段随之清空。 */
export interface SessionDelta {
  base_revision: number
  revision: number
  state: string
  persisted: boolean
  notices?: string[]
  session?: Omit<Session, 'turns'>
  turns?: { from: number; items: Turn[] }
  draft?: {
    len: number
    updates: (
      | { type: 'set'; index: number; part: ContentPart }
      | { type: 'text' | 'reasoning'; index: number; delta: string }
    )[]
  }
  pending_user_question?: UserQuestionRequest | null
}

export interface ModelEntry {
  spec: string
  name: string
  provider: string
  variants: string[]
}

export interface ModelsResponse {
  default: string
  models: ModelEntry[]
}

export interface SkillEntry {
  name: string
  description: string
  source: 'global' | 'project'
  path: string
}

export interface SkillsResponse {
  skills: SkillEntry[]
}

export interface SidebarStats {
  prompt_tokens: number
  completion_tokens: number
  tps?: number | null
  total_tokens: number
  context_tokens: number
  context_limit?: number | null
  cache_read_tokens: number
  cache_write_tokens: number
  estimated_cost_usd: number
  total_turns: number
}

export interface SessionFileResponse {
  path: string
  name: string
  size: number
  is_binary: boolean
  content: string
  mime_type?: string | null
}

export interface RelatedFile {
  path: string
  name: string
  actions: ('read' | 'write' | 'edit' | 'attachment')[]
  turnIndex: number
}

