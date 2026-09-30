import { useEffect, useMemo, useState, type ReactNode } from 'react'
import type { RelatedFile } from '../types'
import { TreeNode } from './file/TreeNode'
import { FilePreview } from './file/FilePreview'
import { useMediaQuery } from '../hooks/useMediaQuery'
import { useFileTree } from '../hooks/useFileTree'
import { fmtBytes, parentDir } from '../utils/highlight'
import { IconFile, IconFolderClosed, IconX } from './icons'

interface FileSidebarProps {
  sessionId: string | null
  workspace?: string | null
  files: RelatedFile[]
  activePath: string | null
  open: boolean
  width: number
  onSelectPath: (path: string | null) => void
  onClose: () => void
}

export function FileSidebar({
  sessionId,
  workspace,
  files,
  activePath,
  open,
  width,
  onSelectPath,
  onClose,
}: FileSidebarProps) {
  const [activeTab, setActiveTab] = useState<'tree' | 'related'>('tree')
  const [filter, setFilter] = useState('')
  const [treeFilter, setTreeFilter] = useState('')
  const isMobile = useMediaQuery('(max-width: 820px)')

  const tree = useFileTree(workspace, open)
  const { rootEntries, rootLoading, rootError, dirChildren, dirLoading, expandedDirs } = tree

  // 树过滤匹配条目（含根与已缓存的子目录）
  const filteredTreeEntries = useMemo(() => {
    if (!treeFilter.trim()) return null
    const query = treeFilter.toLowerCase().trim()
    const all = new Map<string, typeof rootEntries[number]>()
    for (const e of [...rootEntries, ...Object.values(dirChildren).flat()]) {
      all.set(e.path, e)
    }
    return [...all.values()].filter(e => matchesFilter(e, query))
  }, [rootEntries, dirChildren, treeFilter])

  const filteredFiles = useMemo(() => {
    if (!filter.trim()) return files
    const query = filter.toLowerCase().trim()
    return files.filter(f => matchesFilter(f, query))
  }, [files, filter])

  // 预览路径变化时在树中展开其全部上级目录
  useEffect(() => {
    if (activePath) tree.revealPath(activePath)
  }, [activePath, tree.revealPath])

  // 小屏采用“列表 → 预览”两步流，确保文件内容和下载操作都有足够空间。
  const isDualPane = Boolean(activePath && width >= 520 && !isMobile)

  const renderNavList = () => {
    if (activeTab === 'tree') {
      return (
        <>
          <div className="file-sidebar-filter-wrap">
            <input
              type="search"
              className="file-sidebar-filter"
              placeholder="筛选工程文件…"
              value={treeFilter}
              onChange={e => setTreeFilter(e.target.value)}
            />
          </div>

          <div className="file-sidebar-list">
            {rootLoading ? (
              <div className="file-tree-status">
                <span className="file-loading-spinner small" />
                <span>加载工程目录中…</span>
              </div>
            ) : rootError ? (
              <div className="file-tree-status error">
                <span>{rootError}</span>
              </div>
            ) : filteredTreeEntries ? (
              filteredTreeEntries.length === 0 ? (
                <div className="file-empty-state">
                  <IconFile size={36} strokeWidth={1.4} />
                  <div className="file-empty-title">未找到匹配文件</div>
                </div>
              ) : (
                filteredTreeEntries.map(file => (
                  <FileItem
                    key={file.path}
                    file={file}
                    active={file.path === activePath}
                    icon={file.is_dir ? <IconFolderClosed size={16} /> : <IconFile size={16} />}
                    onClick={() => {
                      if (file.is_dir) {
                        tree.expandDir(file.path)
                        setTreeFilter('')
                      } else {
                        onSelectPath(file.path)
                      }
                    }}
                  >
                    {!file.is_dir && <span className="file-tree-size">{fmtBytes(file.size)}</span>}
                  </FileItem>
                ))
              )
            ) : rootEntries.length === 0 ? (
              <div className="file-empty-state">
                <IconFolderClosed size={36} strokeWidth={1.4} />
                <div className="file-empty-title">工程目录为空</div>
              </div>
            ) : (
              <div className="file-tree-view">
                {rootEntries.map(entry => (
                  <TreeNode
                    key={entry.path}
                    entry={entry}
                    depth={0}
                    activePath={activePath}
                    expandedDirs={expandedDirs}
                    dirChildren={dirChildren}
                    dirLoading={dirLoading}
                    onToggleDir={tree.toggleDir}
                    onSelectFile={onSelectPath}
                  />
                ))}
              </div>
            )}
          </div>
        </>
      )
    }

    return (
      <>
        {files.length > 4 && (
          <div className="file-sidebar-filter-wrap">
            <input
              type="search"
              className="file-sidebar-filter"
              placeholder="筛选相关文件…"
              value={filter}
              onChange={e => setFilter(e.target.value)}
            />
          </div>
        )}

        <div className="file-sidebar-list">
          {filteredFiles.length === 0 ? (
            <div className="file-empty-state">
              <IconFile size={36} strokeWidth={1.4} />
              <div className="file-empty-title">
                {filter ? '未找到匹配文件' : '暂无涉及文件'}
              </div>
              <div className="file-empty-hint">
                {filter ? '请尝试其他关键词' : '模型执行 Read、Edit、Write 操作后将自动在此汇总'}
              </div>
            </div>
          ) : (
            filteredFiles.map(file => (
              <FileItem
                key={file.path}
                file={file}
                active={file.path === activePath}
                icon={<IconFile size={16} />}
                onClick={() => onSelectPath(file.path)}
              >
                <div className="file-item-badges">
                  {file.actions.map(action => (
                    <span key={action} className={`file-badge file-badge-${action}`}>
                      {action}
                    </span>
                  ))}
                </div>
              </FileItem>
            ))
          )}
        </div>
      </>
    )
  }

  return (
    <aside
      className={`file-sidebar${isDualPane ? ' dual-pane' : ''}`}
      style={{ width: `${width}px` }}
      aria-label="工程文件浏览器"
      inert={!open}
      aria-hidden={!open}
    >
      {/* 左侧：文件代码预览区（处于双栏模式或单独预览模式时展示） */}
      {(isDualPane || activePath) && (
        <FilePreview
          sessionId={sessionId}
          workspace={workspace}
          activePath={activePath}
          files={files}
          isDualPane={isDualPane}
          onClose={() => onSelectPath(null)}
        />
      )}

      {/* 右侧：工程目录树导航区（双栏时靠右锚定，单栏时占满） */}
      {(isDualPane || !activePath) && (
        <div className="file-sidebar-nav-pane">
          <div className="file-nav-head">
            <div className="file-sidebar-tabs">
              <button
                type="button"
                className={`file-tab-btn${activeTab === 'tree' ? ' active' : ''}`}
                onClick={() => setActiveTab('tree')}
              >
                工程文件
              </button>
              <button
                type="button"
                className={`file-tab-btn${activeTab === 'related' ? ' active' : ''}`}
                onClick={() => setActiveTab('related')}
              >
                会话变动
                {files.length > 0 && (
                  <span className="file-count-badge">{files.length}</span>
                )}
              </button>
            </div>

            <button
              type="button"
              className="icon-ghost file-sidebar-close"
              onClick={onClose}
              title="关闭侧边栏"
              aria-label="关闭侧边栏"
            >
              <IconX size={16} />
            </button>
          </div>

          <div className="file-sidebar-list-container">
            {renderNavList()}
          </div>
        </div>
      )}
    </aside>
  )
}

function matchesFilter(file: { name: string; path: string }, query: string): boolean {
  return file.name.toLowerCase().includes(query) || file.path.toLowerCase().includes(query)
}

/** 文件列表条目：图标 + 名称/父目录 + 尾部附加内容（大小 / 变动徽标） */
function FileItem({
  file,
  active,
  icon,
  onClick,
  children,
}: {
  file: { name: string; path: string }
  active: boolean
  icon: ReactNode
  onClick: () => void
  children?: ReactNode
}) {
  const dir = parentDir(file.path)
  return (
    <button
      type="button"
      className={`file-item${active ? ' active' : ''}`}
      onClick={onClick}
      title={file.path}
    >
      <div className="file-item-icon">{icon}</div>
      <div className="file-item-text">
        <span className="file-item-name">{file.name}</span>
        {dir && <span className="file-item-dir">{dir}</span>}
      </div>
      {children}
    </button>
  )
}
