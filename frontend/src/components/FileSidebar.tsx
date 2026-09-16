import { useEffect, useMemo, useState } from 'react'
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
    for (const e of rootEntries) {
      all.set(e.path, e)
    }
    for (const children of Object.values(dirChildren)) {
      for (const e of children) {
        all.set(e.path, e)
      }
    }
    const matches = (name: string, path: string) =>
      name.toLowerCase().includes(query) || path.toLowerCase().includes(query)
    return [...all.values()].filter(e => matches(e.name, e.path))
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

  if (!open) return null

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
                  <button
                    key={file.path}
                    type="button"
                    className={`file-item${file.path === activePath ? ' active' : ''}`}
                    onClick={() => {
                      if (file.is_dir) {
                        tree.expandDir(file.path)
                        setTreeFilter('')
                      } else {
                        onSelectPath(file.path)
                      }
                    }}
                    title={file.path}
                  >
                    <div className="file-item-icon">
                      {file.is_dir ? <IconFolderClosed size={16} /> : <IconFile size={16} />}
                    </div>
                    <div className="file-item-text">
                      <span className="file-item-name">{file.name}</span>
                      {parentDir(file.path) && (
                        <span className="file-item-dir">{parentDir(file.path)}</span>
                      )}
                    </div>
                    {!file.is_dir && <span className="file-tree-size">{fmtBytes(file.size)}</span>}
                  </button>
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
              <button
                key={file.path}
                type="button"
                className={`file-item${file.path === activePath ? ' active' : ''}`}
                onClick={() => onSelectPath(file.path)}
                title={file.path}
              >
                <div className="file-item-icon">
                  <IconFile size={16} />
                </div>
                <div className="file-item-text">
                  <span className="file-item-name">{file.name}</span>
                  {parentDir(file.path) && (
                    <span className="file-item-dir">{parentDir(file.path)}</span>
                  )}
                </div>
                <div className="file-item-badges">
                  {file.actions.map(action => (
                    <span key={action} className={`file-badge file-badge-${action}`}>
                      {action}
                    </span>
                  ))}
                </div>
              </button>
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
    >
      {/* 左侧：文件代码预览区（处于双栏模式或单独预览模式时展示） */}
      {(isDualPane || (activePath && !isDualPane)) && (
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

function matchesFilter(file: RelatedFile, query: string): boolean {
  return file.name.toLowerCase().includes(query) || file.path.toLowerCase().includes(query)
}
