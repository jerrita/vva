import type { ReactNode } from 'react'
import type { PreviewFile } from '../hooks/usePanelLayout'
import type {
  Annotation,
  ContentPart,
  ImageGenerationCall,
  MediaSource,
  ToolCall,
  ToolResult,
  WebSearchCall,
} from '../types'
import { editDiffs } from '../utils/editDiff'
import type { FileDiff } from '../utils/editDiff'
import { IconChevronRight, IconFile } from './icons'
import { useLightbox } from './Lightbox'
import { Markdown } from './Markdown'
import { baseName, describeSearch, describeTool, externalWebCall, relativePath } from './trace/describe'
import type { Outcome, StepClass, TallyKey, ToolView } from './trace/describe'

/** 图片数据源地址：URL 直连或 Base64 data URL */
export function mediaSourceUrl(source: MediaSource): string {
  return source.kind === 'url'
    ? source.url
    : `data:${source.mime_type};base64,${source.data}`
}

// ---- 节点模型 ----

/** 工具调用与其结果配对后的步骤 */
interface ToolStep {
  kind: 'tool'
  call: ToolCall
  result: ToolResult | null
}

type Step =
  | ToolStep
  | { kind: 'thought'; content: string }
  | { kind: 'search'; call: WebSearchCall }
  | { kind: 'image_gen'; call: ImageGenerationCall }
  | { kind: 'orphan'; result: ToolResult }
  /** 连续同名只读工具（≥3）合并为一行 */
  | { kind: 'batch'; name: string; steps: ToolStep[] }

/** 轨迹段（连续步骤，共用一条轨道）与段间正文 */
type Block =
  | { kind: 'trace'; steps: Step[] }
  | { kind: 'text'; part: Extract<ContentPart, { type: 'text' }> }
  | { kind: 'image'; source: MediaSource }

const BATCHABLE = new Set(['Read', 'Grep', 'Glob'])
const BATCH_MIN = 3

function toSteps(parts: ContentPart[]): (Step | Exclude<Block, { kind: 'trace' }>)[] {
  const out: (Step | Exclude<Block, { kind: 'trace' }>)[] = []
  const tools = new Map<string, ToolStep>()
  for (const part of parts) {
    switch (part.type) {
      case 'text':
        if (part.text.trim()) out.push({ kind: 'text', part })
        break
      case 'image':
        out.push({ kind: 'image', source: part.source })
        break
      case 'reasoning':
        if (part.content.trim()) out.push({ kind: 'thought', content: part.content })
        break
      case 'tool_call': {
        const step: ToolStep = { kind: 'tool', call: part, result: null }
        tools.set(part.id, step)
        out.push(step)
        break
      }
      case 'tool_result': {
        const step = tools.get(part.tool_call_id)
        if (step) step.result = part
        else out.push({ kind: 'orphan', result: part })
        break
      }
      case 'web_search_call':
        out.push({ kind: 'search', call: part })
        break
      case 'image_generation_call':
        out.push({ kind: 'image_gen', call: part })
        break
    }
  }
  return out
}

function isBatchable(step: Step): step is ToolStep {
  return (
    step.kind === 'tool' &&
    BATCHABLE.has(step.call.name) &&
    step.result != null &&
    !step.result.is_error
  )
}

/** 连续同名只读步骤（≥3，均已成功）并为一行 */
function batchSteps(steps: Step[]): Step[] {
  const out: Step[] = []
  let index = 0
  while (index < steps.length) {
    const step = steps[index]
    if (!isBatchable(step)) {
      out.push(step)
      index++
      continue
    }
    const run: ToolStep[] = [step]
    let end = index + 1
    while (end < steps.length) {
      const next = steps[end]
      if (!isBatchable(next) || next.call.name !== step.call.name) break
      run.push(next)
      end++
    }
    if (run.length >= BATCH_MIN) {
      out.push({ kind: 'batch', name: step.call.name, steps: run })
    } else {
      out.push(...run)
    }
    index = end
  }
  return out
}

function toBlocks(parts: ContentPart[]): Block[] {
  const blocks: Block[] = []
  let trace: Step[] | null = null
  for (const node of toSteps(parts)) {
    if (node.kind === 'text' || node.kind === 'image') {
      if (trace) blocks.push({ kind: 'trace', steps: batchSteps(trace) })
      trace = null
      blocks.push(node)
    } else {
      trace ??= []
      trace.push(node)
    }
  }
  if (trace) blocks.push({ kind: 'trace', steps: batchSteps(trace) })
  return blocks
}

// ---- 运行段计数 ----

export interface Tally {
  counts: [TallyKey, number][]
  failed: number
}

const TALLY_ORDER: TallyKey[] = ['查看', '搜索', '修改', '执行', '委派']

/** 工作段的动作计数：按类别汇总，失败单列 */
export function tallyParts(parts: ContentPart[]): Tally {
  const counts = new Map<TallyKey, number>()
  let failed = 0
  const bump = (key: TallyKey) => counts.set(key, (counts.get(key) ?? 0) + 1)
  for (const node of toSteps(parts)) {
    if (node.kind === 'tool') {
      const web = externalWebCall(node.call, node.result)
      bump(web ? '搜索' : describeTool(node.call, node.result, undefined, () => null).tally)
      if (node.result?.is_error) failed++
    } else if (node.kind === 'search') {
      bump('搜索')
      if (node.call.status === 'failed') failed++
    } else if (node.kind === 'orphan' && node.result.is_error) {
      failed++
    }
  }
  return {
    counts: TALLY_ORDER.filter(key => counts.has(key)).map(key => [key, counts.get(key)!]),
    failed,
  }
}

// ---- 行组件 ----

type GlyphState = StepClass | 'thought' | 'running' | 'error'

function Glyph({ state }: { state: GlyphState }) {
  return <span className={`glyph ${state}`} aria-hidden />
}

function OutcomeView({ outcome, error }: { outcome?: Outcome; error: boolean }) {
  if (error) return <span className="step-outcome fail">失败</span>
  if (!outcome) return null
  if ('text' in outcome) return <span className="step-outcome">{outcome.text}</span>
  return (
    <span className="step-outcome">
      <span className="diff-add">+{outcome.plus}</span>
      <span className="diff-del">−{outcome.minus}</span>
    </span>
  )
}

interface LineProps {
  glyph: GlyphState
  verb: string
  target: ReactNode
  mono?: boolean
  secondary?: string
  outcome?: ReactNode
}

/** 末列操作提示：可展开行为箭头，预览行为文件图标 */
type Affordance = 'expand' | 'preview' | null

function StepLine({
  glyph,
  verb,
  target,
  mono,
  secondary,
  outcome,
  affordance,
}: LineProps & { affordance: Affordance }) {
  return (
    <>
      <Glyph state={glyph} />
      <span className="step-verb">{verb}</span>
      <span className="step-target">
        <span className={`step-main${mono ? ' mono' : ''}`}>{target}</span>
        {secondary && <span className="step-secondary">{secondary}</span>}
      </span>
      {outcome ?? <span />}
      <span className="step-affordance" aria-hidden>
        {affordance === 'expand' && <IconChevronRight size={12} strokeWidth={2} />}
        {affordance === 'preview' && <IconFile size={12} strokeWidth={1.8} />}
      </span>
    </>
  )
}

/** 可展开步骤 = details；可点击步骤 = button；其余为静态行 */
function StepRow({
  line,
  body,
  onActivate,
  title,
  tone,
}: {
  line: LineProps
  body?: ReactNode
  onActivate?: () => void
  title?: string
  tone?: 'error'
}) {
  const className = `step${tone ? ` ${tone}` : ''}`
  if (body) {
    return (
      <details className={className}>
        <summary className="step-line" title={title}>
          <StepLine {...line} affordance="expand" />
        </summary>
        <div className="step-body">{body}</div>
      </details>
    )
  }
  if (onActivate) {
    return (
      <button type="button" className={`${className} step-line action`} onClick={onActivate} title={title}>
        <StepLine {...line} affordance="preview" />
      </button>
    )
  }
  return (
    <div className={`${className} step-line`} title={title}>
      <StepLine {...line} affordance={null} />
    </div>
  )
}

function DiffView({
  diffs,
  workspace,
  onPreviewFile,
}: {
  diffs: FileDiff[]
  workspace?: string
  onPreviewFile?: PreviewFile
}) {
  return (
    <div className="diff">
      {diffs.map((diff, index) => (
        <div key={index} className="diff-file">
          <div className="diff-head">
            <span className="diff-path">{relativePath(diff.path, workspace)}</span>
            {diff.op && diff.op !== 'update' && (
              <span className="diff-op">{{ add: '新建', delete: '删除', move: '移动' }[diff.op]}</span>
            )}
            {onPreviewFile && diff.op !== 'delete' && (
              <button type="button" className="diff-open" onClick={() => onPreviewFile(diff.path, 'changes')}>
                <IconFile size={12} strokeWidth={1.8} />
                在侧栏打开
              </button>
            )}
          </div>
          {diff.lines.length > 0 && (
            <pre className="diff-lines">
              {diff.lines.filter(line => !(line.kind === 'hunk' && line.text.trim() === '@@')).map((line, row) => (
                <span key={row} className={`diff-line ${line.kind}`}>
                  {line.kind === 'hunk' ? line.text : line.text || ' '}
                  {'\n'}
                </span>
              ))}
            </pre>
          )}
        </div>
      ))}
    </div>
  )
}

function OutputView({ command, content }: { command?: string; content: string }) {
  return (
    <pre className="step-output">
      {command && <span className="step-command">{command}</span>}
      {content.trim() ? content.replace(/\n+$/, '') : <span className="step-empty">（无输出）</span>}
    </pre>
  )
}

function toolGlyph(view: ToolView, result: ToolResult | null, live: boolean): GlyphState {
  if (result?.is_error) return 'error'
  if (!result && live) return 'running'
  return view.cls
}

function ToolRow({
  step,
  live,
  workspace,
  onPreviewFile,
}: {
  step: ToolStep
  live: boolean
  workspace?: string
  onPreviewFile?: PreviewFile
}) {
  const { call, result } = step
  const web = externalWebCall(call, result)
  if (web) return <SearchRow call={web} />

  const diffs = call.name === 'Edit' || call.name === 'Write' ? editDiffs(call.name, call.arguments) : []
  const view = describeTool(call, result, workspace, () =>
    diffs.length > 0
      ? {
          plus: diffs.reduce((sum, diff) => sum + diff.plus, 0),
          minus: diffs.reduce((sum, diff) => sum + diff.minus, 0),
        }
      : null,
  )
  const error = result?.is_error ?? false
  const line: LineProps = {
    glyph: toolGlyph(view, result, live),
    verb: view.verb,
    target: view.target,
    mono: view.mono,
    secondary: view.secondary,
    outcome: <OutcomeView outcome={view.outcome} error={error} />,
  }

  let body: ReactNode = null
  if (error && result) {
    body = <OutputView command={view.command} content={result.content} />
  } else if (view.body === 'diff' && diffs.length > 0) {
    body = <DiffView diffs={diffs} workspace={workspace} onPreviewFile={onPreviewFile} />
  } else if (view.body === 'output' && result) {
    body = <OutputView command={view.command} content={result.content} />
  }

  const preview = view.filePath && onPreviewFile ? () => onPreviewFile(view.filePath!, view.range ?? null) : undefined
  return (
    <StepRow
      line={line}
      body={body}
      onActivate={preview}
      title={preview ? `在侧栏预览 ${view.filePath}` : view.command}
      tone={error ? 'error' : undefined}
    />
  )
}

function BatchRow({
  name,
  steps,
  workspace,
  onPreviewFile,
}: {
  name: string
  steps: ToolStep[]
  workspace?: string
  onPreviewFile?: PreviewFile
}) {
  const views = steps.map(step => describeTool(step.call, step.result, workspace, () => null))
  const names = views.map(view => (view.filePath ? baseName(view.filePath) : view.target))
  const unit = name === 'Read' ? '个文件' : '次'
  return (
    <StepRow
      line={{
        glyph: 'observe',
        verb: name,
        target: `${steps.length} ${unit}`,
        secondary: names.join('  '),
      }}
      body={
        <div className="step-batch">
          {steps.map((step, index) => (
            <ToolRow key={index} step={step} live={false} workspace={workspace} onPreviewFile={onPreviewFile} />
          ))}
        </div>
      }
    />
  )
}

/** 思考首行预览：去 Markdown 标记；流式中取最新一行（跑马灯） */
function thoughtPreview(content: string, streaming: boolean): string {
  const lines = content
    .split('\n')
    .map(line => line.replace(/^#+\s*|\*\*|__|`/g, '').trim())
    .filter(Boolean)
  return (streaming ? lines.at(-1) : lines[0]) ?? ''
}

function ThoughtRow({ content, streaming }: { content: string; streaming: boolean }) {
  return (
    <StepRow
      line={{
        glyph: streaming ? 'running' : 'thought',
        verb: 'Think',
        target: <span className="thought-preview">{thoughtPreview(content, streaming)}</span>,
      }}
      body={
        <div className="thought-body">
          <Markdown text={content} />
        </div>
      }
    />
  )
}

function SearchRow({ call }: { call: WebSearchCall }) {
  const { verb, target, outcome } = describeSearch(call)
  const running = call.status === 'searching' || call.status === 'pending'
  const failed = call.status === 'failed'
  const sources = call.sources ?? []
  const url = call.action?.type === 'open_page' ? call.action.url : null
  return (
    <StepRow
      line={{
        glyph: failed ? 'error' : running ? 'running' : 'observe',
        verb,
        target: url ? (
          <a href={url} target="_blank" rel="noreferrer" onClick={event => event.stopPropagation()}>
            {target}
          </a>
        ) : (
          target
        ),
        outcome: <OutcomeView outcome={outcome ? { text: outcome } : undefined} error={failed} />,
      }}
      body={
        sources.length > 0 ? (
          <ol className="step-sources">
            {sources.map((source, index) => (
              <li key={index}>
                <a href={source.url} target="_blank" rel="noreferrer">
                  {source.title || source.url}
                </a>
                <span className="source-host">{hostOf(source.url)}</span>
              </li>
            ))}
          </ol>
        ) : undefined
      }
      tone={failed ? 'error' : undefined}
    />
  )
}

function hostOf(url: string): string {
  try {
    return new URL(url).host.replace(/^www\./, '')
  } catch {
    return ''
  }
}

function ImageGenRow({ call }: { call: ImageGenerationCall }) {
  const openLightbox = useLightbox()
  const url = call.result ? mediaSourceUrl(call.result) : null
  return (
    <StepRow
      line={{
        glyph: call.status === 'in_progress' ? 'running' : call.status === 'incomplete' ? 'error' : 'change',
        verb: 'Image',
        target: call.revised_prompt ?? '生成图片',
      }}
      body={
        url ? (
          <img className="part-image lightboxable" src={url} alt="" onClick={() => openLightbox(url)} />
        ) : undefined
      }
    />
  )
}

function TraceView({
  steps,
  live,
  workspace,
  onPreviewFile,
}: {
  steps: Step[]
  /** 本段处于流式末尾：未决工具为运行态，末尾思考滚动预览 */
  live: boolean
  workspace?: string
  onPreviewFile?: PreviewFile
}) {
  return (
    <div className="trace">
      {steps.map((step, index) => {
        switch (step.kind) {
          case 'tool':
            return <ToolRow key={index} step={step} live={live} workspace={workspace} onPreviewFile={onPreviewFile} />
          case 'batch':
            return (
              <BatchRow key={index} name={step.name} steps={step.steps} workspace={workspace} onPreviewFile={onPreviewFile} />
            )
          case 'thought':
            return <ThoughtRow key={index} content={step.content} streaming={live && index === steps.length - 1} />
          case 'search':
            return <SearchRow key={index} call={step.call} />
          case 'image_gen':
            return <ImageGenRow key={index} call={step.call} />
          case 'orphan':
            return (
              <StepRow
                key={index}
                line={{ glyph: step.result.is_error ? 'error' : 'execute', verb: 'Result', target: step.result.tool_call_id, mono: true }}
                body={<OutputView content={step.result.content} />}
                tone={step.result.is_error ? 'error' : undefined}
              />
            )
        }
      })}
    </div>
  )
}

function Citations({ annotations }: { annotations: Annotation[] }) {
  // 与 TUI citation_ref 一致：url_citation 与 web_search_result_location 均展示，按 URL 去重
  type CitedAnnotation = Extract<Annotation, { type: 'url_citation' | 'web_search_result_location' }>
  const seen = new Set<string>()
  const citations = annotations.filter((a): a is CitedAnnotation => {
    if ((a.type !== 'url_citation' && a.type !== 'web_search_result_location') || seen.has(a.url)) return false
    seen.add(a.url)
    return true
  })
  if (citations.length === 0) return null
  return (
    <details className="citations">
      <summary>引用来源（{citations.length}）</summary>
      <ol>
        {citations.map((citation, index) => (
          <li key={index}>
            <a href={citation.url} target="_blank" rel="noreferrer">
              {citation.title || citation.url}
            </a>
            <span className="source-host">{hostOf(citation.url)}</span>
          </li>
        ))}
      </ol>
    </details>
  )
}

function ImagePart({ source }: { source: MediaSource }) {
  const openLightbox = useLightbox()
  const url = mediaSourceUrl(source)
  return <img className="part-image lightboxable" src={url} alt="" onClick={() => openLightbox(url)} />
}

/**
 * 部件序列 → 正文与轨迹段交替排布：连续的思考/工具/搜索共用一条轨道，正文打断轨道。
 * tool_call 与同 tool_call_id 的 tool_result 配对为一步；未配对的 result 独立成步。
 * 所有步骤默认收起，流式中不自动展开（避免内容跳动）；live 仅影响运行态字形与思考跑马灯。
 */
export function Parts({
  parts,
  live = false,
  workspace,
  onPreviewFile,
}: {
  parts: ContentPart[]
  live?: boolean
  /** 会话 workspace：项目内文件展示为相对路径 */
  workspace?: string
  onPreviewFile?: PreviewFile
}) {
  const blocks = toBlocks(parts)
  return (
    <>
      {blocks.map((block, index) => {
        if (block.kind === 'trace') {
          return (
            <TraceView
              key={index}
              steps={block.steps}
              live={live && index === blocks.length - 1}
              workspace={workspace}
              onPreviewFile={onPreviewFile}
            />
          )
        }
        if (block.kind === 'image') return <ImagePart key={index} source={block.source} />
        return (
          <div key={index} className="part-text">
            <Markdown text={block.part.text} />
            {block.part.annotations && block.part.annotations.length > 0 && (
              <Citations annotations={block.part.annotations} />
            )}
          </div>
        )
      })}
    </>
  )
}
