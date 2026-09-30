import type { Session, SidebarStats } from '../types'
import { CopyButton } from './CopyButton'

function fmtNum(value: number): string {
  return value.toLocaleString()
}

/** 上下文窗口上限缩写，对齐 TUI format_context_limit */
function fmtLimit(value: number): string {
  if (value >= 1_000_000 && value % 1_000_000 === 0) return `${value / 1_000_000}M`
  return `${Math.floor(value / 1000)}k`
}

const DEFAULT_CONTEXT_LIMIT = 200_000

/** 上下文占用比例（0–1，封顶 1） */
export function contextRatio(stats: SidebarStats): number {
  return Math.min(1, stats.context_tokens / (stats.context_limit ?? DEFAULT_CONTEXT_LIMIT))
}

/** 上下文摘要文案：`97,292 / 500k` */
export function contextLabel(stats: SidebarStats): string {
  return `${fmtNum(stats.context_tokens)} / ${fmtLimit(stats.context_limit ?? DEFAULT_CONTEXT_LIMIT)}`
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <div className="stat-row">
      <span className="stat-label">{label}</span>
      <span className="stat-value">{value}</span>
    </div>
  )
}

/** 用量统计弹层：锚定于顶栏开关下方，三组字段 + 底部会话信息。 */
export function StatsPanel({ stats, active }: { stats: SidebarStats; active: Session }) {
  const percent = Math.round(contextRatio(stats) * 100)
  const level = percent >= 90 ? ' danger' : percent >= 70 ? ' warning' : ''

  return (
    <div className="stats-popover" role="dialog" aria-label="用量统计" data-stats-popover>
      <section className="stats-section">
        <div className="stats-context">{contextLabel(stats)}</div>
        <div className="ctx-gauge">
          <span className="ctx-gauge-track">
            <span className={`ctx-gauge-fill${level}`} style={{ width: `${percent}%` }} />
          </span>
          <span>{percent}%</span>
        </div>
      </section>
      <section className="stats-section">
        <Row label="In" value={fmtNum(stats.prompt_tokens)} />
        <Row label="Out" value={fmtNum(stats.completion_tokens)} />
        <Row label="Cache read" value={fmtNum(stats.cache_read_tokens)} />
        <Row label="Cache write" value={fmtNum(stats.cache_write_tokens)} />
        {stats.tps != null && <Row label="TPS" value={stats.tps.toFixed(1)} />}
      </section>
      <section className="stats-section">
        <Row label="Turns" value={fmtNum(stats.total_turns)} />
        <Row label="Editor" value={active.editor ?? 'replace'} />
        <Row label="Est. cost" value={`$${stats.estimated_cost_usd.toFixed(4)}`} />
      </section>
      <div className="stats-foot">
        <span className="stats-foot-line" title={active.workspace_root}>
          {active.id} · {active.workspace_root}
        </span>
        <CopyButton text={active.id} />
      </div>
    </div>
  )
}
