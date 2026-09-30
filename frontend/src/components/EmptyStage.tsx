import { ChipSelect } from './ChipSelect'
import { Composer } from './Composer'
import { IconFolderOpen } from './icons'
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

/** 空态：无会话时的居中版式（品牌大字 + 项目选择与路径 + 输入框）。 */
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
  const root = draftProject || defaultRoot || ''
  return (
    <div className="empty-stage">
      <div className="empty-brand">vva</div>
      <div className="empty-composer">
        <div className="empty-project">
          <IconFolderOpen size={15} />
          <ChipSelect
            title="切换项目"
            value={root}
            options={projects.map(project => ({ value: project.root, label: project.name }))}
            onChange={onDraftProjectChange}
          />
          {root && (
            <span className="empty-project-path" title={root}>
              {root}
            </span>
          )}
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
