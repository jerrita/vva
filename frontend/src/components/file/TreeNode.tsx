import { fmtBytes } from '../../utils/highlight'
import {
  IconChevronDown,
  IconChevronRight,
  IconFile,
  IconFolderClosed,
  IconFolderOpen,
} from '../icons'
import type { FsEntry } from '../../types'

interface TreeNodeProps {
  entry: FsEntry
  depth: number
  activePath: string | null
  expandedDirs: Set<string>
  dirChildren: Record<string, FsEntry[]>
  dirLoading: Record<string, boolean>
  onToggleDir: (path: string) => void
  onSelectFile: (path: string) => void
}

export function TreeNode({
  entry,
  depth,
  activePath,
  expandedDirs,
  dirChildren,
  dirLoading,
  onToggleDir,
  onSelectFile,
}: TreeNodeProps) {
  const isExpanded = expandedDirs.has(entry.path)
  const isLoading = !!dirLoading[entry.path]
  const children = dirChildren[entry.path] || []
  const isActive = entry.path === activePath

  if (entry.is_dir) {
    return (
      <div className="file-tree-node">
        <button
          type="button"
          className="file-tree-row dir"
          style={{ paddingLeft: `${depth * 14 + 8}px` }}
          onClick={() => onToggleDir(entry.path)}
          title={entry.path}
        >
          <span className="file-tree-chevron">
            {isExpanded ? <IconChevronDown size={14} /> : <IconChevronRight size={14} />}
          </span>
          <span className="file-tree-icon dir">
            {isExpanded ? <IconFolderOpen size={15} /> : <IconFolderClosed size={15} />}
          </span>
          <span className="file-tree-name">{entry.name}</span>
          {isLoading && <span className="file-loading-spinner small" />}
        </button>
        {isExpanded && (
          <div className="file-tree-children">
            {children.length === 0 && !isLoading ? (
              <div
                className="file-tree-empty"
                style={{ paddingLeft: `${(depth + 1) * 14 + 22}px` }}
              >
                空目录
              </div>
            ) : (
              children.map(child => (
                <TreeNode
                  key={child.path}
                  entry={child}
                  depth={depth + 1}
                  activePath={activePath}
                  expandedDirs={expandedDirs}
                  dirChildren={dirChildren}
                  dirLoading={dirLoading}
                  onToggleDir={onToggleDir}
                  onSelectFile={onSelectFile}
                />
              ))
            )}
          </div>
        )}
      </div>
    )
  }

  return (
    <button
      type="button"
      className={`file-tree-row file${isActive ? ' active' : ''}`}
      style={{ paddingLeft: `${depth * 14 + 22}px` }}
      onClick={() => onSelectFile(entry.path)}
      title={entry.path}
    >
      <span className="file-tree-icon file">
        <IconFile size={15} />
      </span>
      <span className="file-tree-name">{entry.name}</span>
      <span className="file-tree-size">{fmtBytes(entry.size)}</span>
    </button>
  )
}
