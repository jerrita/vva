import { Fragment } from 'react'
import type { RefObject } from 'react'
import type { Session, SidebarStats } from '../types'

function fmtNum(value: number): string {
  return value.toLocaleString()
}

/** 上下文窗口上限缩写，对齐 TUI format_context_limit */
function fmtLimit(value: number): string {
  if (value >= 1_000_000 && value % 1_000_000 === 0) return `${value / 1_000_000}M`
  return `${Math.floor(value / 1000)}k`
}

interface StatsPanelProps {
  stats: SidebarStats
  active: Session
  visible: boolean
  panelRef: RefObject<HTMLDivElement | null>
}

/** 用量统计浮块：非阻塞，字段行 + Context 比例条。 */
export function StatsPanel({ stats, active, visible, panelRef }: StatsPanelProps) {
  const workspaceName =
    active.workspace_root.split('/').filter(Boolean).pop() ?? active.workspace_root ?? null
  const contextLimit = stats.context_limit ?? 200_000
  const contextPercent = Math.min(
    100,
    Math.round((stats.context_tokens / contextLimit) * 100),
  )

  return (
    <div className={`stats-panel${visible ? '' : ' hidden'}`} ref={panelRef}>
      {(
        [
          ['Session', active.id],
          ['Workspace', workspaceName],
          ['Editor', active.editor ?? 'replace'],
          ['Turns', fmtNum(stats.total_turns)],
          ['Context', `${fmtNum(stats.context_tokens)} / ${fmtLimit(contextLimit)}`],
          ['In', fmtNum(stats.prompt_tokens)],
          ['Out', fmtNum(stats.completion_tokens)],
          ...(stats.tps != null ? ([['TPS', stats.tps.toFixed(1)]] as const) : []),
          ['Cache Read', fmtNum(stats.cache_read_tokens)],
          ['Cache Write', fmtNum(stats.cache_write_tokens)],
          ['Est. Cost', `$${stats.estimated_cost_usd.toFixed(4)}`],
        ] as [string, string][]
      ).map(([label, value]) => (
        <Fragment key={label}>
          <div className="stat-row">
            <span className="stat-label">{label}</span>
            <span className="stat-value">{value}</span>
          </div>
          {label === 'Context' && (
            <div className="ctx-gauge">
              <span className="ctx-gauge-track">
                <span className="ctx-gauge-fill" style={{ width: `${contextPercent}%` }} />
              </span>
              <span>{contextPercent}%</span>
            </div>
          )}
        </Fragment>
      ))}
    </div>
  )
}
