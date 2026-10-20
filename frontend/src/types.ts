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
  action?: WebSearchAction
  status: SearchStatus
  sources?: WebSearchSource[]
}

export type ImageGenerationStatus = 'in_progress' | 'completed' | 'incomplete'

export interface ImageGenerationCall {
  id: string
  status: ImageGenerationStatus
  result?: MediaSource
  revised_prompt?: string
}

export interface ToolCall {
  id: string
  name: string
  /** Function 调用为 JSON 文本；kind 为 custom 时是 freeform 原文（补丁） */
  arguments: string
  kind?: 'custom'
}

export interface ToolResult {
  tool_call_id: string
  background_task_id?: string
  task?: BackgroundTask
  content: string
  attachments?: MediaContent[]
  is_error: boolean
}

export interface BackgroundTask {
  task_id: string
  status: 'running' | 'completed' | 'failed' | 'stopped' | 'interrupted'
  output: string
  exit_code?: number
  total_bytes?: number
  truncated: boolean
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
  | { type: 'file_ref'; path: string; content: string; truncated: boolean }
  | ({ type: 'tool_call' } & ToolCall)
  | ({ type: 'tool_result' } & ToolResult)
  | ({ type: 'web_search_call' } & WebSearchCall)
  | ({ type: 'image_generation_call' } & ImageGenerationCall)

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
  background_task?: BackgroundTask
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
  /** 父会话 id：subagent 会话指向派生它的会话 */
  parent?: string | null
  /** 派生本会话的 agent 定义名（subagent 会话携带） */
  agent?: string | null
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
  /** 父会话 id：subagent 会话指向派生它的会话 */
  parent?: string | null
  /** 派生本会话的 agent 定义名（subagent 会话携带） */
  agent?: string | null
  /** 运行状态文案；空或缺席表示空闲 */
  state?: string
}

export interface SessionSnapshot {
  revision: number
  state: string
  persisted: boolean
  notices: Notice[]
  background_tasks: Record<string, BackgroundTask>
  /** 外置 web 工具接管的后端名；空表示原生在位或未开启 */
  external_web: string[]
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
  notices?: Notice[]
  background_tasks?: Record<string, BackgroundTask>
  external_web?: string[]
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
  /** 文本或图片 Base64；音频和 PDF 通过 preview 接口读取，此处为空。 */
  content: string
  mime_type?: string | null
}

/** 会话涉及的文件（前端派生类型，非后端镜像） */
export interface RelatedFile {
  /** 最近一次调用中模型所写的原始路径（文件 API 可直接接受） */
  path: string
  /** 工作区相对路径，用于与文件树匹配 */
  rel: string
  name: string
  dir: string
  actions: ('read' | 'write' | 'edit' | 'attachment')[]
  plus: number
  minus: number
  deleted: boolean
  /** 读取过的行区间（1-based，闭区间） */
  readRanges: { start: number; end: number }[]
  turnIndex: number
  /** 全局单调递增的最近触达序号，用于按新近度排序 */
  lastIndex: number
}

export interface FsEntry {
  name: string
  path: string
  is_dir: boolean
  size: number
}

export interface FsEntriesResponse {
  workspace: string
  path: string
  entries: FsEntry[]
}

export interface FsFindEntry extends FsEntry {
  /** `path` 中命中的字符下标（按 Unicode 码点计），用于高亮 */
  indices: number[]
}

export interface FsFindResponse {
  workspace: string
  query: string
  entries: FsFindEntry[]
  truncated: boolean
}


export type Notice = { id: string; text: string } & (
  | { kind: 'session' }
  | { kind: 'capability' }
  | { kind: 'turn'; turn_id: string; aborted: boolean }
)
