import { ChipSelect } from './ChipSelect'
import { Composer } from './Composer'
import type { ReactNode } from 'react'
import type { FileAttachment, SkillEntry } from '../types'
import type { ProjectOption } from './Sidebar'

interface EmptyStageProps {
  projects: ProjectOption[]
  defaultRoot: string | null
  draftProject: string
  onDraftProjectChange: (project: string) => void
  controls: ReactNode
  running: boolean
  skills: SkillEntry[]
  onSend: (text: string, files: FileAttachment[]) => void
  onStop: () => void
}

/** 空态：无会话时的居中版式（品牌大字 + 提示 + 项目选择 + 输入框）。 */
export function EmptyStage({
  projects,
  defaultRoot,
  draftProject,
  onDraftProjectChange,
  controls,
  running,
  skills,
  onSend,
  onStop,
}: EmptyStageProps) {
  return (
    <div className="empty-stage">
      <div className="empty-brand">vva</div>
      <div className="empty-hint">选择项目，输入任务开始 · Enter 发送</div>
      <div className="empty-composer">
        <div className="empty-project">
          <ChipSelect
            title="项目"
            value={draftProject || defaultRoot || ''}
            options={projects.map(project => ({ value: project.root, label: project.name }))}
            onChange={onDraftProjectChange}
          />
        </div>
        <Composer
          running={running}
          onSend={onSend}
          onStop={onStop}
          controls={controls}
          skills={skills}
          workspace={draftProject || defaultRoot || undefined}
        />
      </div>
    </div>
  )
}
